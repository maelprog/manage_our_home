# Regenerates apps/web/src/brand/ (#542). Not run by the build: the files it
# writes are committed, and the binary `include_bytes!`s them. Three steps,
# each in a throwaway container, from the repo root, with OUT an empty
# scratch directory and DEPS one for the Python packages:
#
#   docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$DEPS:/deps" \
#     python:3.13-slim pip install -q --target /deps fonttools brotli uharfbuzz pillow
#   docker run --rm -u "$(id -u):$(id -g)" -v "$DEPS:/deps:ro" -v "$OUT:/out" \
#     -v "$PWD/apps/web/assets/fonts:/font:ro" -v "$PWD/apps/web/src/brand/tools:/t:ro" \
#     python:3.13-slim python -I /t/draw.py
#   docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$OUT:/out" \
#     -v "$PWD/apps/web/src/brand/tools:/t:ro" \
#     mcr.microsoft.com/playwright:v1.61.1-noble bash /t/rasterize.sh
#   docker run --rm -u "$(id -u):$(id -g)" -v "$DEPS:/deps:ro" -v "$OUT:/out" \
#     -v "$PWD/apps/web/src/brand/tools:/t:ro" python:3.13-slim python -I /t/pack.py
#
# then copy favicon.svg, favicon.ico, apple-touch-icon.png, icon-192.png,
# icon-512.png, icon-maskable-512.png, logo.svg and logo-mono.svg from OUT.
#
# This step draws the SVGs: the A5 symbol, and the "MHome" wordmark turned
# into outlines (no <text>: an SVG used as an image or a favicon loads no web
# font) from the self-hosted Fraunces, pinned at wght 600 and opsz 24, shaped
# by HarfBuzz so the kerning is the font's own.
import sys; sys.path.insert(0, "/deps")
import io
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
import uharfbuzz as hb

OUT = "/out"
ACCENT = "#14706E"
FG = "#1C1A17"

# The A5 symbol, verbatim from the issue (#542, comment 6415675537).
SYMBOL = (
    '<path d="M6.5 24.5V14.5L11.25 9L16 14.5L20.75 9L25.5 14.5V24.5" fill="none" stroke="{c}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>'
    '<path d="M19.25 24.5V20.5a1.5 1.5 0 0 1 3 0V24.5" fill="none" stroke="{c}" stroke-width="2.2" stroke-linecap="round"/>'
    '<rect x="9.5" y="17" width="3.5" height="3.5" rx=".6" fill="{c}"/>'
)
NS = 'xmlns="http://www.w3.org/2000/svg"'


def tile(rx="7.5", inner=""):
    return f'<rect width="32" height="32" rx="{rx}" fill="{ACCENT}"/>' + (inner or SYMBOL.format(c="#FFFFFF"))


def write(name, s):
    with open(f"{OUT}/{name}", "w") as f:
        f.write(s + "\n")


# --- favicon / app icons ---------------------------------------------------
write("favicon.svg", f'<svg viewBox="0 0 32 32" {NS}>{tile()}</svg>')
# Full-bleed square: iOS rounds the corners itself, and draws black under
# transparency.
write("apple-touch-icon.svg", f'<svg viewBox="0 0 32 32" {NS}>{tile(rx="0")}</svg>')
# Maskable: full bleed, the symbol scaled to 80 % about the centre so its
# farthest point stays inside the 40 %-radius safe circle.
write(
    "icon-maskable.svg",
    f'<svg viewBox="0 0 32 32" {NS}><rect width="32" height="32" fill="{ACCENT}"/>'
    f'<g transform="translate(16 16) scale(.8) translate(-16 -16)">{SYMBOL.format(c="#FFFFFF")}</g></svg>',
)

# --- the wordmark: "MHome", Fraunces 600, opsz 24, shaped by HarfBuzz ------
font = TTFont("/font/fraunces-v1.000.woff2")
inst = instantiateVariableFont(font, {"wght": 600, "opsz": 24})
buf = io.BytesIO()
inst.flavor = None
inst.save(buf)
data = buf.getvalue()
upem = inst["head"].unitsPerEm
cap = inst["OS/2"].sCapHeight

hbface = hb.Face(data)
hbfont = hb.Font(hbface)
b = hb.Buffer()
b.add_str("MHome")
b.guess_segment_properties()
hb.shape(hbfont, b, {"kern": True, "liga": True})
glyphset = inst.getGlyphSet()
order = inst.getGlyphOrder()

# Wordmark sized so its cap height is 14 units of the 32-unit tile, caps
# centred on the tile: cap top at y = 9, baseline at y = 23.
CAP_UNITS = 14.0
scale = CAP_UNITS / cap
X0, BASE = 40.0, 23.0


def wordmark_path():
    pen = SVGPathPen(glyphset, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
    x = 0
    for info, pos in zip(b.glyph_infos, b.glyph_positions):
        name = order[info.codepoint]
        t = TransformPen(pen, (scale, 0, 0, -scale, X0 + (x + pos.x_offset) * scale, BASE - pos.y_offset * scale))
        glyphset[name].draw(t)
        x += pos.x_advance
    return pen.getCommands(), X0 + x * scale


d, right = wordmark_path()
width = round(right + 1, 1)
print("wordmark right edge", right, "cap", cap, "upem", upem, "path bytes", len(d))

write(
    "logo.svg",
    f'<svg viewBox="0 0 {width} 32" {NS}><title>MHome</title>{tile()}<path fill="{FG}" d="{d}"/></svg>',
)
write(
    "logo-mono.svg",
    f'<svg viewBox="0 0 {width} 32" {NS}><title>MHome</title>{SYMBOL.format(c="currentColor")}<path fill="currentColor" d="{d}"/></svg>',
)
