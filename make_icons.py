"""Generates assets/icon.ico (ON), assets/icon_off.ico (OFF), assets/logo.png"""
from PIL import Image, ImageDraw

S = 1024          # master render size
K = S / 256       # scale from 256-unit design grid

def sc(v): return int(round(v * K))

def gradient(c1, c2):
    g = Image.new("RGB", (256, 256))
    px = g.load()
    for y in range(256):
        for x in range(256):
            t = (x + y) / 510
            px[x, y] = tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))
    return g.resize((S, S), Image.BICUBIC).convert("RGBA")

def render(on=True):
    if on:
        bg = gradient((108, 92, 231), (0, 210, 255))   # violet -> cyan
        pane_alpha = 245
    else:
        bg = gradient((70, 75, 90), (40, 43, 54))      # slate
        pane_alpha = 120

    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [sc(8), sc(8), sc(248), sc(248)], radius=sc(56), fill=255)

    # glossy sheen on the top half
    sheen = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sheen)
    for y in range(sc(130)):
        sd.line([0, y, S, y], fill=(255, 255, 255, int(46 * (1 - y / sc(130)))))
    bg = Image.alpha_composite(bg, sheen)

    # snapped-window panes: one tall left, two stacked right
    panes = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(panes)
    col = (255, 255, 255, pane_alpha)
    r = sc(14)
    d.rounded_rectangle([sc(44), sc(56), sc(122), sc(200)], r, fill=col)
    d.rounded_rectangle([sc(134), sc(56), sc(212), sc(122)], r, fill=col)
    d.rounded_rectangle([sc(134), sc(134), sc(212), sc(200)], r, fill=col)

    if not on:  # "disabled" slash
        d.line([sc(48), sc(212), sc(208), sc(44)], fill=(255, 90, 90, 255), width=sc(14))

    out = Image.alpha_composite(bg, panes)
    out.putalpha(mask)
    return out

sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
on, off = render(True), render(False)
on.save("assets/icon.ico", sizes=sizes)
off.save("assets/icon_off.ico", sizes=sizes)
on.resize((512, 512), Image.LANCZOS).save("assets/logo.png")
print("ok")
