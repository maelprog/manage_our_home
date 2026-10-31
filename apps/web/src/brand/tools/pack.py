# Step 3 of draw.py: PNGs re-encoded, the two full-bleed ones made opaque,
# favicon.ico packed from the 16 and 32 px renders.
import sys; sys.path.insert(0, "/deps")
from PIL import Image

O = "/out"
for name in ["apple-touch-icon", "icon-192", "icon-512", "icon-maskable-512", "favicon-32", "favicon-16"]:
    im = Image.open(f"{O}/{name}.png").convert("RGBA")
    print(name, im.size)
    im.save(f"{O}/{name}.png", optimize=True)
# iOS paints transparency black: the touch icon is opaque.
Image.open(f"{O}/apple-touch-icon.png").convert("RGB").save(f"{O}/apple-touch-icon.png", optimize=True)
Image.open(f"{O}/icon-maskable-512.png").convert("RGB").save(f"{O}/icon-maskable-512.png", optimize=True)
big = Image.open(f"{O}/favicon-32.png")
small = Image.open(f"{O}/favicon-16.png")
big.save(f"{O}/favicon.ico", format="ICO", sizes=[(16, 16), (32, 32)], append_images=[small])
check = Image.open(f"{O}/favicon.ico")
print("ico sizes", check.info.get("sizes"))
