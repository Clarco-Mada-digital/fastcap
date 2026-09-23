#!/usr/bin/env python3
"""Génère l'icône source de FastCap (RGBA 1024x1024).

Tauri exige une icône RGBA (et non en palette) pour la zone de notification.
Usage :
    python3 scripts/generate-icon.py
    npx tauri icon src-tauri/icons/source.png
"""

from PIL import Image, ImageDraw

SIZE = 1024
SCALE = 4
W = SIZE * SCALE

CYAN = (34, 211, 238, 255)
CYAN_SOFT = (34, 211, 238, 210)
DARK_TOP = (26, 31, 48)
DARK_BOTTOM = (14, 17, 27)


def linear_gradient(size: int, top: tuple, bottom: tuple) -> Image.Image:
    grad = Image.new("RGBA", (1, size))
    for y in range(size):
        t = y / max(size - 1, 1)
        grad.putpixel(
            (0, y),
            (
                int(top[0] + (bottom[0] - top[0]) * t),
                int(top[1] + (bottom[1] - top[1]) * t),
                int(top[2] + (bottom[2] - top[2]) * t),
                255,
            ),
        )
    return grad.resize((size, size))


def build_icon() -> Image.Image:
    canvas = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    # Fond : carré arrondi avec dégradé sombre
    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, W - 1, W - 1], radius=int(W * 0.22), fill=255
    )
    canvas.paste(linear_gradient(W, DARK_TOP, DARK_BOTTOM), (0, 0), mask)

    draw = ImageDraw.Draw(canvas)

    # Cadre de sélection : quatre équerres cyan
    margin = int(W * 0.24)
    arm = int(W * 0.17)
    thick = int(W * 0.045)
    right = W - margin
    bottom = W - margin

    brackets = [
        [(margin, margin + arm), (margin, margin), (margin + arm, margin)],
        [(right - arm, margin), (right, margin), (right, margin + arm)],
        [(margin, bottom - arm), (margin, bottom), (margin + arm, bottom)],
        [(right - arm, bottom), (right, bottom), (right, bottom - arm)],
    ]
    for points in brackets:
        draw.line(points, fill=CYAN, width=thick, joint="curve")

    # Obturateur central
    cx = cy = W // 2
    outer = int(W * 0.155)
    ring = int(W * 0.028)

    draw.ellipse(
        [cx - outer, cy - outer, cx + outer, cy + outer],
        outline=CYAN_SOFT,
        width=ring,
    )
    inner = int(W * 0.075)
    draw.ellipse([cx - inner, cy - inner, cx + inner, cy + inner], fill=CYAN)

    return canvas.resize((SIZE, SIZE), Image.LANCZOS)


if __name__ == "__main__":
    icon = build_icon()
    icon.save("src-tauri/icons/source.png")
    # Icône de la zone de notification : doit être RGBA
    icon.resize((512, 512), Image.LANCZOS).save("src-tauri/icons/icon.png")
    print("Icônes générées : src-tauri/icons/source.png et icon.png")
