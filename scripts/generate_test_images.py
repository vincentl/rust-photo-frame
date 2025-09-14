#!/usr/bin/env python3
"""Generate placeholder safari animal images for tests.

This script produces a mix of PNG and JPEG files with varying
dimensions and orientations. It relies only on the Python standard
library. To regenerate JPEG images, first compile ``ppm_to_jpeg.c``::

    gcc scripts/ppm_to_jpeg.c -ljpeg -o ppm_to_jpeg

The script expects the ``ppm_to_jpeg`` binary in the project root.
"""
import math, struct, zlib, os

# basic drawing canvas
class Canvas:
    def __init__(self, w, h):
        self.w, self.h = w, h
        self.pixels = [[(135,206,235) for _ in range(w)] for _ in range(h)]  # sky
        ground_y = int(h*0.75)
        for y in range(ground_y, h):
            for x in range(w):
                self.pixels[y][x] = (189,183,107)  # khaki ground
    def set(self, x, y, color):
        if 0 <= x < self.w and 0 <= y < self.h:
            self.pixels[y][x] = color
    def rect(self, x0, y0, x1, y1, color):
        for y in range(max(0,y0), min(self.h,y1)):
            for x in range(max(0,x0), min(self.w,x1)):
                self.pixels[y][x] = color
    def circle(self, cx, cy, r, color):
        for y in range(cy-r, cy+r):
            for x in range(cx-r, cx+r):
                if 0 <= x < self.w and 0 <= y < self.h:
                    if (x-cx)**2 + (y-cy)**2 <= r*r:
                        self.pixels[y][x] = color
    def line(self, x0, y0, x1, y1, color):
        dx = abs(x1-x0); dy = -abs(y1-y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        while True:
            self.set(x0, y0, color)
            if x0 == x1 and y0 == y1: break
            e2 = 2*err
            if e2 >= dy:
                err += dy; x0 += sx
            if e2 <= dx:
                err += dx; y0 += sy

# png writer

def chunk(tag, data):
    return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag+data) & 0xffffffff)

def write_png(path, canvas):
    w, h = canvas.w, canvas.h
    raw = b''
    for row in canvas.pixels:
        raw += b'\x00' + bytes([c for rgb in row for c in rgb])
    comp = zlib.compress(raw)
    with open(path, 'wb') as f:
        f.write(b'\x89PNG\r\n\x1a\n')
        f.write(chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b'IDAT', comp))
        f.write(chunk(b'IEND', b''))

def write_ppm(path, canvas):
    w, h = canvas.w, canvas.h
    with open(path, 'wb') as f:
        f.write(f'P6\n{w} {h}\n255\n'.encode())
        for row in canvas.pixels:
            f.write(bytes([c for rgb in row for c in rgb]))

# animal drawing helpers

def draw_lion(c):
    cx, cy = c.w//2, int(c.h*0.6)
    r = min(c.w, c.h)//8
    c.circle(cx, cy, r+8, (160,82,45))  # mane
    c.circle(cx, cy, r, (238,173,45))

def draw_elephant(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    gray = (105,105,105)
    c.rect(bx, by, bx+body_w, by+body_h, gray)
    c.rect(bx-body_w//3, by, bx, by+body_h, gray)  # head
    c.rect(bx-body_w//3- body_w//6, by+body_h//3, bx-body_w//3, by+body_h//3+body_h//2, gray)  # trunk
    c.circle(bx-body_w//6, by+body_h//3, body_h//3, (169,169,169))

def draw_giraffe(c):
    body_w = c.w//4; body_h = c.h//4
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    yellow = (218,165,32); brown=(139,69,19)
    c.rect(bx, by, bx+body_w, by+body_h, yellow)
    neck_h = body_h
    c.rect(bx+body_w//2 - body_w//8, by-neck_h, bx+body_w//2 + body_w//8, by, yellow)
    c.rect(bx+body_w//2 - body_w//8, by-neck_h-body_w//4, bx+body_w//2 + body_w//8, by-neck_h, yellow)  # head
    # spots
    for i in range(5):
        c.rect(bx + (i*body_w)//5, by + (i%2)*body_h//2, bx + (i*body_w)//5 + body_w//10, by + (i%2)*body_h//2 + body_h//10, brown)


def draw_zebra(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    c.rect(bx, by, bx+body_w, by+body_h, (255,255,255))
    # stripes
    for i in range(0, body_w, body_w//6):
        c.rect(bx+i, by, bx+i+body_w//12, by+body_h, (0,0,0))

def draw_rhino(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    gray=(112,128,144)
    c.rect(bx, by, bx+body_w, by+body_h, gray)
    c.rect(bx+body_w, by+body_h//3, bx+body_w+body_w//6, by+body_h//3+body_h//6, gray)  # head
    c.rect(bx+body_w+body_w//6, by+body_h//3, bx+body_w+body_w//4, by+body_h//3+body_h//8, (192,192,192))  # horn

def draw_buffalo(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    dark=(60,40,20)
    c.rect(bx, by, bx+body_w, by+body_h, dark)
    # horns
    horn_w = body_w//4; horn_h = body_h//4
    c.rect(bx-horn_w, by, bx, by+horn_h, (245,245,245))
    c.rect(bx+body_w, by, bx+body_w+horn_w, by+horn_h, (245,245,245))

def draw_cheetah(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    yellow=(255,215,0); black=(0,0,0)
    c.rect(bx, by, bx+body_w, by+body_h, yellow)
    for i in range(12):
        x = bx + (i*body_w)//12
        y = by + (i%3)*body_h//3
        c.circle(x, y, body_h//10, black)

def draw_hyena(c):
    body_w = c.w//3; body_h = c.h//5
    bx = c.w//2 - body_w//2; by = int(c.h*0.55)
    brown=(160,82,45); dark=(101,67,33)
    c.rect(bx, by, bx+body_w, by+body_h, brown)
    c.rect(bx+body_w//3, by, bx+body_w//3*2, by+body_h//2, dark)

def draw_leopard(c):
    body_w = c.w//3; body_h = c.h//5
    bx=c.w//2 - body_w//2; by=int(c.h*0.55)
    gold=(218,165,32); brown=(139,69,19); black=(0,0,0)
    c.rect(bx,by,bx+body_w,by+body_h,gold)
    for i in range(10):
        x=bx + (i*body_w)//10 + body_w//20
        y=by + (i%2)*body_h//2 + body_h//4
        c.circle(x,y,body_h//10,brown)
        c.circle(x,y,body_h//14,black)

def draw_hippo(c):
    water_h = c.h//8
    for y in range(c.h-water_h, c.h):
        for x in range(c.w):
            c.pixels[y][x] = (65,105,225)
    body_w=c.w//3; body_h=c.h//5
    bx=c.w//2-body_w//2; by=int(c.h*0.55)
    purple=(147,112,219)
    c.rect(bx,by,bx+body_w,by+body_h,purple)

def draw_wildebeest(c):
    body_w=c.w//3; body_h=c.h//5
    bx=c.w//2-body_w//2; by=int(c.h*0.55)
    dark=(70,70,70)
    c.rect(bx,by,bx+body_w,by+body_h,dark)
    horn_w=body_w//6
    c.rect(bx-horn_w,by,bx,by+body_h//4,(200,200,200))
    c.rect(bx+body_w,by,bx+body_w+horn_w,by+body_h//4,(200,200,200))

def draw_ostrich(c):
    body_r = min(c.w,c.h)//10
    cx, cy = c.w//2, int(c.h*0.6)
    c.circle(cx, cy, body_r, (0,0,0))
    c.rect(cx-body_r//2, cy-body_r*2, cx+body_r//2, cy-body_r, (255,255,255))  # tail
    c.rect(cx, cy-body_r*3, cx+body_r//3, cy-body_r*2, (255,255,255))  # neck
    c.circle(cx+body_r//3, cy-body_r*3, body_r//3, (255,255,255))

animals = [
    ("lion_square.png", 1024,1024, draw_lion, 'png'),
    ("elephant_square.jpg",512,512, draw_elephant, 'jpg'),
    ("giraffe_portrait.png",720,1280, draw_giraffe, 'png'),
    ("zebra_portrait.jpg",1080,1920, draw_zebra, 'jpg'),
    ("rhino_landscape.png",1280,720, draw_rhino, 'png'),
    ("buffalo_landscape.jpg",1920,1080, draw_buffalo, 'jpg'),
    ("cheetah_portrait.png",750,1334, draw_cheetah, 'png'),
    ("hyena_landscape.jpg",1600,900, draw_hyena, 'jpg'),
    ("leopard_square.png",600,600, draw_leopard, 'png'),
    ("hippo_square.jpg",1000,1000, draw_hippo, 'jpg'),
    ("wildebeest_landscape.png",1200,800, draw_wildebeest, 'png'),
    ("ostrich_portrait.jpg",800,1200, draw_ostrich, 'jpg')
]

out_dir = os.path.join(os.path.dirname(__file__), '..', 'tests', 'images')
os.makedirs(out_dir, exist_ok=True)

for name,w,h,fn,fmt in animals:
    c = Canvas(w,h)
    fn(c)
    path = os.path.join(out_dir, name)
    if fmt == 'png':
        write_png(path, c)
    else:
        ppm_path = path + '.ppm'
        write_ppm(ppm_path, c)
        os.system(os.path.join(os.path.dirname(__file__), '..', 'ppm_to_jpeg') + f' {ppm_path} {path}')
        os.remove(ppm_path)
