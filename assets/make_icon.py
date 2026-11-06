# Draws the X Finder app icon (assets/xfinder.ico, assets/xfinder.png).
# Run: python assets/make_icon.py
from PIL import Image, ImageDraw, ImageFilter
import os

S = 1024
here = os.path.dirname(os.path.abspath(__file__))

def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))

def draw():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # Blue gradient squircle.
    grad = Image.new("RGBA", (S, S))
    gd = ImageDraw.Draw(grad)
    for y in range(S):
        gd.line([(0, y), (S, y)], fill=lerp((86, 170, 255, 255), (24, 84, 222, 255), y / S))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([56, 56, S - 56, S - 56], radius=230, fill=255)
    # Soft shadow under the tile.
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([64, 80, S - 64, S - 40], radius=230, fill=(0, 0, 0, 90))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(18)))
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)
    # Folder: back tab, then body.
    d.rounded_rectangle([222, 292, 520, 430], radius=46, fill=(214, 233, 255, 255))
    d.rounded_rectangle([222, 352, 802, 762], radius=66, fill=(255, 255, 255, 255))
    return img

big = draw()
big.resize((256, 256), Image.LANCZOS).save(os.path.join(here, "xfinder.png"))
sizes = [256, 128, 64, 48, 40, 32, 24, 20, 16]
frames = [big.resize((s, s), Image.LANCZOS) for s in sizes]
frames[0].save(os.path.join(here, "xfinder.ico"), sizes=[(s, s) for s in sizes], append_images=frames[1:])
print("ok")
