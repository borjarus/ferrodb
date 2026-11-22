"""Regenerate raster icons from the SVG masters.

    pip install pillow resvg-py
    python assets/icons/generate.py

Sizes <= 32 px are rendered from ferrodb-small.svg, the rest from ferrodb.svg.
"""
import io
from pathlib import Path

import resvg_py
from PIL import Image

NAME = "ferrodb"
HERE = Path(__file__).resolve().parent
BIG = (HERE / f"{NAME}.svg").read_text(encoding="utf-8")
SMALL = (HERE / f"{NAME}-small.svg").read_text(encoding="utf-8")
SIZES = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def render(px: int) -> Image.Image:
    svg = SMALL if px <= 32 else BIG
    data = bytes(resvg_py.svg_to_bytes(svg_string=svg, width=px, height=px))
    return Image.open(io.BytesIO(data)).convert("RGBA")


def main() -> None:
    (HERE / "png").mkdir(exist_ok=True)
    imgs = {px: render(px) for px in SIZES}
    for px, img in imgs.items():
        img.save(HERE / "png" / f"{px}x{px}.png")
    imgs[1024].save(HERE / f"{NAME}.png")
    imgs[256].save(
        HERE / f"{NAME}.ico",
        sizes=[(s, s) for s in ICO_SIZES],
        append_images=[imgs[s] for s in ICO_SIZES[:-1]],
    )
    imgs[1024].save(HERE / f"{NAME}.icns")


if __name__ == "__main__":
    main()
