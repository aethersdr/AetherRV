# Builds web/img/logo*.png: the AetherSDR logo (../AetherSDR/docs/assets/logo-circle-256.png) with an RV silhouette
# over its lower part. Run from the repo root: python3 tools/make_logo_rv.py
from PIL import Image, ImageDraw, ImageFilter
SRC = "/home/jeremy/AetherSDR/docs/assets/logo-circle-256.png"
K = 4                                  # supersample
base = Image.open(SRC).convert("RGBA").resize((256 * K, 256 * K), Image.LANCZOS)
W = 256 * K
# hide the droplet: soft dark patch sampled from the circle's background colour
bg = base.getpixel((128 * K, 236 * K))
patch = Image.new("RGBA", base.size, (0, 0, 0, 0)); d = ImageDraw.Draw(patch)
d.ellipse([(128 - 22) * K, (176 - 14) * K, (128 + 22) * K, (226 + 4) * K], fill=bg[:3] + (255,))
base = Image.alpha_composite(base, patch.filter(ImageFilter.GaussianBlur(6 * K)))
# RV side profile (Sprinter high roof, facing right), in 256-px units, centred near the bottom
def P(pts, ox=128 - 46, oy=176):
    return [((ox + x) * K, (oy + y) * K) for x, y in pts]
rv = Image.new("RGBA", base.size, (0, 0, 0, 0)); g = ImageDraw.Draw(rv)
body = [(4, 6), (60, 6), (66, 8), (72, 14), (84, 22), (90, 25), (92, 30), (92, 37), (2, 37), (2, 10)]
col, glow = (214, 250, 255, 255), (120, 225, 240, 255)
g.polygon(P(body), fill=col)
g.rounded_rectangle([*(P([(2, 6)])[0]), *(P([(64, 37)])[0])], radius=4 * K, fill=col)
dark = (16, 34, 52, 255)
for x0, x1 in ((8, 22), (26, 40), (44, 56)):                       # side windows
    g.rounded_rectangle([*(P([(x0, 11)])[0]), *(P([(x1, 19)])[0])], radius=2 * K, fill=dark)
g.polygon(P([(64, 11), (69, 11), (80, 22), (64, 22)]), fill=dark)  # windscreen
g.line(P([(2, 30), (92, 30)]), fill=(150, 220, 235, 255), width=int(1.2 * K))  # lower trim
for cx in (20, 74):                                                  # wheels
    c = P([(cx, 38)])[0]
    g.ellipse([c[0] - 7.5 * K, c[1] - 7.5 * K, c[0] + 7.5 * K, c[1] + 7.5 * K], fill=dark, outline=col, width=int(2 * K))
    g.ellipse([c[0] - 2.5 * K, c[1] - 2.5 * K, c[0] + 2.5 * K, c[1] + 2.5 * K], fill=col)
# glow under it, matching the logo's light
halo = rv.split()[3].filter(ImageFilter.GaussianBlur(5 * K))
glow_l = Image.new("RGBA", base.size, glow[:3] + (0,)); glow_l.putalpha(halo.point(lambda a: int(a * .55)))
out = Image.alpha_composite(Image.alpha_composite(base, glow_l), rv)
# keep the circle mask of the original
mask = Image.open(SRC).convert("RGBA").resize((W, W), Image.LANCZOS).split()[3]
out.putalpha(mask)
big = out.resize((256, 256), Image.LANCZOS)
big.save("web/img/logo-rv-256.png")
big.resize((192, 192), Image.LANCZOS).save("web/img/logo.png", optimize=True)
big.resize((64, 64), Image.LANCZOS).save("web/img/favicon.png", optimize=True)
print("ok")
