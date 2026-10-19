import ctypes, ctypes.wintypes as wt, time
from PIL import Image
u, g = ctypes.windll.user32, ctypes.windll.gdi32
u.SetProcessDPIAware()
wins = []
@ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
def cb(h, _):
    if u.IsWindowVisible(h):
        b = ctypes.create_unicode_buffer(256); u.GetWindowTextW(h, b, 256)
        if b.value == "d2rs": wins.append(h)
    return True
u.EnumWindows(cb, 0)
print(len(wins), "d2rs windows")
def grab(h):
    r = wt.RECT(); u.GetClientRect(h, ctypes.byref(r)); w, hh = r.right, r.bottom
    dc = u.GetDC(h); mdc = g.CreateCompatibleDC(dc); bmp = g.CreateCompatibleBitmap(dc, w, hh); g.SelectObject(mdc, bmp)
    ok = u.PrintWindow(h, mdc, 3)   # PW_CLIENTONLY | PW_RENDERFULLCONTENT
    class BI(ctypes.Structure): _fields_ = [("s", wt.DWORD), ("w", ctypes.c_long), ("h", ctypes.c_long), ("p", wt.WORD), ("b", wt.WORD), ("c", wt.DWORD), ("x", wt.DWORD*5)]
    bi = BI(ctypes.sizeof(BI), w, -hh, 1, 32, 0); buf = ctypes.create_string_buffer(w*hh*4)
    g.GetDIBits(mdc, bmp, 0, hh, buf, ctypes.byref(bi), 0)
    g.DeleteObject(bmp); g.DeleteDC(mdc); u.ReleaseDC(h, dc)
    return ok, Image.frombuffer("RGBA", (w, hh), buf, "raw", "BGRA", 0, 1).convert("RGB")
t = time.time()
ims = [grab(h) for h in wins[:6]]
print("captured", len(ims), "in %.2fs" % (time.time()-t), [ (ok, im.size, im.getextrema()[0]) for ok, im in ims])
W = 640; tiles = [im.resize((W, W*im.height//im.width)) for _, im in ims]
if tiles:
    sheet = Image.new("RGB", (W*3, tiles[0].height*2))
    for i, im in enumerate(tiles): sheet.paste(im, ((i%3)*W, (i//3)*tiles[0].height))
    sheet.save("cap_sheet.jpg")
