// FastCap - Exécution de processus externes sous contrainte de temps
//
// Toute la détection de matériel (ffmpeg, ffprobe, caméras, micros) passe par
// ici. Un périphérique occupé ou un pilote capricieux ne doit jamais figer
// l'interface : chaque appel est borné dans le temps et le processus est tué
// s'il dépasse son délai.

use std::io::Read;
use std::process::{Child, Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Intervalle de scrutation de la terminaison d'un processus
const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Exécute une commande et indique si elle a réussi avant l'échéance.
pub fn run_bounded(cmd: &mut Command, limit: Duration) -> bool {
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };

    match wait_bounded(&mut child, limit) {
        Some(status) => status.success(),
        None => {
            kill(&mut child);
            false
        }
    }
}

/// Exécute une commande et récupère sa sortie, ou `None` si le délai expire.
pub fn run_output_bounded(cmd: &mut Command, limit: Duration) -> Option<Output> {
    let mut child = cmd.spawn().ok()?;

    if wait_bounded(&mut child, limit).is_none() {
        kill(&mut child);
        return None;
    }

    child.wait_with_output().ok()
}

/// Attend la fin d'un processus, au plus `limit`.
fn wait_bounded(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }

        if start.elapsed() >= limit {
            return None;
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Termine un processus et récupère son code de sortie (évite les zombies).
pub fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Tampon circulaire recueillant les dernières lignes d'erreur d'un processus.
///
/// La version précédente redirigeait `stderr` de ffmpeg vers `/dev/null` :
/// quand l'encodage échouait, l'utilisateur ne recevait qu'un laconique
/// « l'encodage n'a produit aucun fichier ». On conserve désormais la fin du
/// journal pour l'afficher tel quel.
/// Écho produit par ffmpeg à chaque commande interactive.
///
/// Piloter l'incrustation en direct génère deux de ces lignes par commande :
/// sans les écarter, elles chassaient du tampon les véritables messages
/// d'erreur, seuls utiles à l'utilisateur.
fn is_interactive_echo(line: &str) -> bool {
    line.starts_with("Enter command:")
        || line.starts_with("Command reply")
        || line.starts_with("Parse error")
}

#[derive(Clone, Default)]
pub struct StderrTail {
    lines: Arc<Mutex<Vec<String>>>,
}

impl StderrTail {
    /// Nombre de lignes conservées
    const CAPACITY: usize = 80;

    pub fn new() -> Self {
        Self::default()
    }

    /// Consomme `stderr` du processus dans un thread dédié.
    ///
    /// Indispensable : sans lecture, un processus bavard finit par saturer le
    /// tube et se bloquer définitivement.
    pub fn attach(&self, child: &mut Child) {
        let Some(stderr) = child.stderr.take() else {
            return;
        };

        let lines = self.lines.clone();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stderr);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];

            while let Ok(read) = reader.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);

                // Découpage en lignes au fil de l'eau
                while let Some(position) = buffer.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=position).collect();
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    if text.is_empty() || is_interactive_echo(&text) {
                        continue;
                    }
                    if let Ok(mut guard) = lines.lock() {
                        guard.push(text);
                        let excess = guard.len().saturating_sub(Self::CAPACITY);
                        if excess > 0 {
                            guard.drain(..excess);
                        }
                    }
                }
            }
        });
    }

    /// Les dernières lignes recueillies, prêtes à être affichées.
    pub fn tail(&self, lines: usize) -> String {
        let guard = match self.lines.lock() {
            Ok(guard) => guard,
            Err(_) => return String::new(),
        };

        let start = guard.len().saturating_sub(lines);
        guard[start..].join(" | ")
    }

    /// Message d'erreur enrichi du journal de ffmpeg, quand il y en a un.
    pub fn explain(&self, context: &str) -> String {
        let detail = self.tail(4);
        if detail.is_empty() {
            context.to_string()
        } else {
            format!("{context} — ffmpeg: {detail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[test]
    fn bounded_run_reports_success() {
        assert!(run_bounded(
            Command::new("true").stdout(Stdio::null()),
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn bounded_run_reports_failure() {
        assert!(!run_bounded(
            Command::new("false").stdout(Stdio::null()),
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn bounded_run_gives_up_on_a_hanging_process() {
        let started = Instant::now();
        let finished = run_bounded(
            Command::new("sleep").arg("30").stdout(Stdio::null()),
            Duration::from_millis(200),
        );

        assert!(!finished);
        // Le processus a bien été tué au lieu d'être attendu 30 secondes
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_binary_is_not_a_panic() {
        assert!(!run_bounded(
            &mut Command::new("fastcap-binaire-inexistant"),
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let tail = StderrTail::new();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("echo première >&2; echo seconde >&2")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("processus de test");

        tail.attach(&mut child);
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(150));

        let captured = tail.tail(4);
        assert!(captured.contains("seconde"), "obtenu: {captured}");
    }
}
