// FastCap - Installation assistée des dépendances externes
//
// FastCap s'appuie sur deux binaires système : `ffmpeg` (enregistrement vidéo)
// et `tesseract` (OCR, facultatif). Demander à l'utilisateur de les installer
// à la main puis de « les ajouter au PATH » n'est raisonnable que pour un
// public averti. Ce module détecte le gestionnaire de paquets de la machine et
// lance l'installation à sa place.
//
// L'élévation de privilèges passe par `pkexec`, qui affiche la demande de mot
// de passe du système : l'utilisateur voit et confirme ce qui va être installé.
// La commande est entièrement construite ici, jamais à partir d'une saisie.

use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process_util::{run_bounded, run_output_bounded};

/// Une dépendance externe et son état
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// "ffmpeg" | "tesseract"
    pub id: String,
    pub name: String,
    pub description: String,
    pub installed: bool,
    pub version: Option<String>,
    /// L'application sait-elle l'installer sur cette machine ?
    pub installable: bool,
    /// Commande qui sera exécutée, affichée à l'utilisateur avant validation
    pub install_command: Option<String>,
    /// Indispensable au fonctionnement, ou simple confort ?
    pub required: bool,
}

/// Gestionnaire de paquets détecté.
///
/// Les variantes sont construites derrière des `cfg` de plateforme : sous
/// Linux, `Winget` et `Homebrew` restent inutilisées.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Winget,
    Homebrew,
}

impl PackageManager {
    fn binary(self) -> &'static str {
        match self {
            PackageManager::Apt => "apt-get",
            PackageManager::Dnf => "dnf",
            PackageManager::Pacman => "pacman",
            PackageManager::Zypper => "zypper",
            PackageManager::Winget => "winget",
            PackageManager::Homebrew => "brew",
        }
    }

    /// Arguments d'installation, sans interaction clavier
    fn install_args(self, package: &str) -> Vec<String> {
        match self {
            PackageManager::Apt => vec!["install".into(), "-y".into(), package.into()],
            PackageManager::Dnf => vec!["install".into(), "-y".into(), package.into()],
            PackageManager::Pacman => {
                vec!["-S".into(), "--noconfirm".into(), "--needed".into(), package.into()]
            }
            PackageManager::Zypper => {
                vec!["--non-interactive".into(), "install".into(), package.into()]
            }
            PackageManager::Winget => vec![
                "install".into(),
                "--id".into(),
                package.into(),
                "-e".into(),
                "--accept-source-agreements".into(),
                "--accept-package-agreements".into(),
            ],
            PackageManager::Homebrew => vec!["install".into(), package.into()],
        }
    }

    /// L'installation exige-t-elle une élévation de privilèges ?
    fn needs_root(self) -> bool {
        !matches!(self, PackageManager::Winget | PackageManager::Homebrew)
    }
}

fn binary_exists(name: &str) -> bool {
    // La commande doit vivre dans une liaison nommée : `Command::new(..).arg(..)`
    // rend une référence au temporaire, libéré en fin d'instruction.
    #[cfg(target_os = "windows")]
    let mut command = Command::new("where");
    #[cfg(target_os = "windows")]
    let probe = command.arg(name).stdout(Stdio::null()).stderr(Stdio::null());

    #[cfg(not(target_os = "windows"))]
    let mut command = Command::new("sh");
    #[cfg(not(target_os = "windows"))]
    let probe = command
        .arg("-c")
        .arg(format!("command -v {name}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    run_bounded(probe, Duration::from_secs(3))
}

/// Gestionnaire de paquets utilisable sur cette machine
fn detect_package_manager() -> Option<PackageManager> {
    #[cfg(target_os = "linux")]
    {
        for candidate in [
            PackageManager::Apt,
            PackageManager::Dnf,
            PackageManager::Pacman,
            PackageManager::Zypper,
        ] {
            if binary_exists(candidate.binary()) {
                return Some(candidate);
            }
        }
        None
    }

    #[cfg(target_os = "windows")]
    {
        binary_exists("winget").then_some(PackageManager::Winget)
    }

    #[cfg(target_os = "macos")]
    {
        binary_exists("brew").then_some(PackageManager::Homebrew)
    }
}

/// Nom du paquet selon la distribution
fn package_name(manager: PackageManager, dependency: &str) -> &'static str {
    match (dependency, manager) {
        ("ffmpeg", PackageManager::Winget) => "Gyan.FFmpeg",
        ("ffmpeg", _) => "ffmpeg",
        ("tesseract", PackageManager::Winget) => "UB-Mannheim.TesseractOCR",
        ("tesseract", PackageManager::Apt) => "tesseract-ocr",
        ("tesseract", PackageManager::Homebrew) => "tesseract",
        ("tesseract", _) => "tesseract",
        _ => "",
    }
}

/// Programme d'élévation disponible (`pkexec`), le cas échéant
#[cfg(target_os = "linux")]
fn elevator() -> Option<&'static str> {
    binary_exists("pkexec").then_some("pkexec")
}

#[cfg(not(target_os = "linux"))]
fn elevator() -> Option<&'static str> {
    None
}

/// Commande complète d'installation : programme + arguments
fn install_plan(dependency: &str) -> Option<(String, Vec<String>)> {
    let manager = detect_package_manager()?;
    let package = package_name(manager, dependency);
    if package.is_empty() {
        return None;
    }

    let mut args = vec![manager.binary().to_string()];
    args.extend(manager.install_args(package));

    if manager.needs_root() {
        // Sans élévation graphique, l'installation ne pourrait pas aboutir
        let elevator = elevator()?;
        Some((elevator.to_string(), args))
    } else {
        let program = args.remove(0);
        Some((program, args))
    }
}

/// Version courte d'un binaire, pour l'afficher
fn binary_version(name: &str) -> Option<String> {
    let output = run_output_bounded(
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        Duration::from_secs(5),
    )?;

    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };

    text.lines().next().map(|line| line.trim().to_string())
}

/// État des dépendances externes de l'application
pub fn status() -> Vec<Dependency> {
    let definitions: &[(&str, &str, &str, bool)] = &[
        (
            "ffmpeg",
            "ffmpeg",
            "Capture et encodage vidéo. Indispensable pour enregistrer l'écran.",
            true,
        ),
        (
            "tesseract",
            "Tesseract OCR",
            "Extraction du texte contenu dans une capture. Facultatif.",
            false,
        ),
    ];

    definitions
        .iter()
        .map(|(id, name, description, required)| {
            let installed = binary_exists(id);
            let plan = install_plan(id);

            Dependency {
                id: (*id).to_string(),
                name: (*name).to_string(),
                description: (*description).to_string(),
                version: installed.then(|| binary_version(id)).flatten(),
                installed,
                installable: !installed && plan.is_some(),
                install_command: plan.map(|(program, args)| {
                    format!("{program} {}", args.join(" "))
                }),
                required: *required,
            }
        })
        .collect()
}

/// Résultat d'une tentative d'installation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub success: bool,
    pub message: String,
    /// Sortie de l'installateur, utile en cas d'échec
    pub log: String,
}

/// Installe une dépendance via le gestionnaire de paquets du système.
///
/// L'appel est bloquant et peut durer plusieurs minutes (téléchargement).
/// `pkexec` affiche lui-même la demande d'authentification.
pub fn install(dependency: &str) -> InstallOutcome {
    // Liste blanche : seules ces dépendances peuvent être installées, ce qui
    // évite qu'un identifiant arbitraire ne se retrouve dans une commande.
    if !matches!(dependency, "ffmpeg" | "tesseract") {
        return InstallOutcome {
            success: false,
            message: format!("Dépendance inconnue : {dependency}"),
            log: String::new(),
        };
    }

    if binary_exists(dependency) {
        return InstallOutcome {
            success: true,
            message: format!("{dependency} est déjà installé."),
            log: String::new(),
        };
    }

    let Some((program, args)) = install_plan(dependency) else {
        return InstallOutcome {
            success: false,
            message: no_installer_message(),
            log: String::new(),
        };
    };

    let output = run_output_bounded(
        Command::new(&program)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        // Téléchargement compris : on laisse largement le temps
        Duration::from_secs(600),
    );

    let Some(output) = output else {
        return InstallOutcome {
            success: false,
            message: "L'installation n'a pas abouti dans le temps imparti.".to_string(),
            log: String::new(),
        };
    };

    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .trim()
    .to_string();

    // C'est la présence réelle du binaire qui fait foi, pas le code de sortie :
    // certains gestionnaires renvoient un code non nul pour un simple
    // avertissement.
    if binary_exists(dependency) {
        return InstallOutcome {
            success: true,
            message: format!("{dependency} a été installé."),
            log,
        };
    }

    InstallOutcome {
        success: false,
        message: if output.status.code() == Some(126) {
            "Installation annulée (authentification refusée).".to_string()
        } else {
            format!("L'installation de {dependency} a échoué.")
        },
        log: tail(&log, 1500),
    }
}

fn no_installer_message() -> String {
    #[cfg(target_os = "linux")]
    {
        if detect_package_manager().is_none() {
            return "Aucun gestionnaire de paquets reconnu (apt, dnf, pacman, zypper). \
                    Installez ffmpeg avec les outils de votre distribution."
                .to_string();
        }
        "L'installation automatique demande « pkexec » (paquet policykit-1), absent de ce système."
            .to_string()
    }

    #[cfg(target_os = "windows")]
    {
        "L'installation automatique demande « winget », absent de ce système.".to_string()
    }

    #[cfg(target_os = "macos")]
    {
        "L'installation automatique demande Homebrew : voir https://brew.sh".to_string()
    }
}

/// Conserve la fin d'un texte, pour ne pas inonder l'interface
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }

    let start = text.len() - max;
    // On repart d'une frontière de caractère valide
    let start = text
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= start)
        .unwrap_or(0);

    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lists_ffmpeg_as_required() {
        let dependencies = status();
        let ffmpeg = dependencies
            .iter()
            .find(|d| d.id == "ffmpeg")
            .expect("ffmpeg listé");

        assert!(ffmpeg.required);
    }

    #[test]
    fn tesseract_is_optional() {
        let dependencies = status();
        let tesseract = dependencies
            .iter()
            .find(|d| d.id == "tesseract")
            .expect("tesseract listé");

        assert!(!tesseract.required);
    }

    #[test]
    fn an_installed_dependency_is_not_offered_for_install() {
        for dependency in status() {
            if dependency.installed {
                assert!(
                    !dependency.installable,
                    "{} est installé mais proposé à l'installation",
                    dependency.id
                );
            }
        }
    }

    #[test]
    fn unknown_dependencies_are_refused() {
        let outcome = install("rm -rf /");
        assert!(!outcome.success);
        assert!(outcome.message.contains("inconnue"));
    }

    #[test]
    fn package_names_differ_per_platform() {
        assert_eq!(package_name(PackageManager::Apt, "tesseract"), "tesseract-ocr");
        assert_eq!(package_name(PackageManager::Winget, "ffmpeg"), "Gyan.FFmpeg");
    }

    #[test]
    fn root_is_only_required_for_system_package_managers() {
        assert!(PackageManager::Apt.needs_root());
        assert!(!PackageManager::Homebrew.needs_root());
        assert!(!PackageManager::Winget.needs_root());
    }

    #[test]
    fn tail_keeps_the_end_and_valid_utf8() {
        let text = "é".repeat(2000);
        let kept = tail(&text, 100);
        assert!(kept.len() <= 110);
        assert!(kept.starts_with('…'));
    }

    #[test]
    fn a_known_binary_is_detected() {
        assert!(binary_exists("sh") || binary_exists("cmd"));
        assert!(!binary_exists("fastcap-binaire-qui-nexiste-pas"));
    }
}
