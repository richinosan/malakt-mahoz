#!/usr/bin/env python3
"""背景の画像（青い空、森）を作る。写真は使わず、ノイズと稜線で描く。

使い方: python3 scripts/scenes.py crates/app/assets/scenes
"""
import sys

import numpy as np
from PIL import Image, ImageFilter

W, H = 1280, 720


def noise(seed, octaves=5, base=4):
    """なめらかなノイズ（0〜1）。粗い乱数を引き伸ばして重ねる。"""
    rng = np.random.default_rng(seed)
    out = np.zeros((H, W))
    total = 0.0
    for o in range(octaves):
        n = base * 2**o
        grid = rng.random((n * H // W + 2, n + 2)).astype(np.float32)
        layer = np.asarray(
            Image.fromarray((grid * 255).astype(np.uint8)).resize((W, H), Image.BICUBIC)
        )
        weight = 0.5**o
        out += layer / 255.0 * weight
        total += weight
    return out / total


def gradient(top, bottom, power=1.0):
    t = (np.linspace(0, 1, H) ** power)[:, None, None]
    return np.array(top)[None, None, :] * (1 - t) + np.array(bottom)[None, None, :] * t + np.zeros((H, W, 3))


def blend(img, color, alpha):
    return img * (1 - alpha[..., None]) + np.array(color)[None, None, :] * alpha[..., None]


def sky(night):
    if night:
        img = gradient((6, 14, 38), (28, 52, 96), 1.2)
        rng = np.random.default_rng(7)
        stars = np.zeros((H, W))
        ys = (rng.random(420) ** 1.6 * H * 0.8).astype(int)
        xs = rng.integers(0, W, 420)
        stars[ys, xs] = rng.random(420) * 0.9 + 0.1
        stars = np.asarray(Image.fromarray((stars * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(0.8))) / 255.0
        img = blend(img, (235, 240, 255), np.clip(stars * 3, 0, 1))
        cloud, tone, amount = noise(3), (70, 96, 150), 0.55
    else:
        img = gradient((38, 120, 214), (176, 216, 248), 0.9)
        cloud, tone, amount = noise(3), (255, 255, 255), 0.95
    # 雲は下のほうに多く、上は晴れている
    rise = np.linspace(0.25, 1.0, H)[:, None]
    alpha = np.clip((cloud - 0.5) * 4.2, 0, 1) * rise * amount
    return blend(img, tone, alpha)


def forest(night):
    if night:
        img = gradient((14, 38, 36), (66, 112, 94), 1.4)
        mist, layers = (120, 170, 146), [(48, 88, 74), (30, 64, 54), (16, 40, 34), (6, 20, 17)]
    else:
        img = gradient((214, 232, 220), (236, 244, 232), 1.0)
        mist, layers = (240, 247, 240), [(170, 200, 180), (124, 164, 140), (78, 122, 98), (44, 84, 64)]
    xs = np.arange(W)
    ys = np.arange(H)[:, None]
    for i, color in enumerate(layers):
        rng = np.random.default_rng(20 + i)
        # 稜線に、木の先のとがりを重ねる
        ridge = noise(40 + i, octaves=3, base=3)[0] * 150
        period = 34 - i * 4
        phase = rng.random() * period
        spike = np.abs(((xs + phase) % period) / period - 0.5) * 2
        height = (rng.random(W // period + 2) * 0.6 + 0.4)[((xs + phase) // period).astype(int)]
        top = H * (0.42 + i * 0.13) + ridge - (1 - spike) * (46 + i * 16) * height
        inside = (ys >= top[None, :]).astype(np.float32)
        img = blend(img, color, inside)
        # 奥の層ほど霧がかかる
        haze = np.clip(1 - (ys - top[None, :]) / 220, 0, 1) * inside * (0.34 - i * 0.07)
        img = blend(img, mist, np.clip(haze, 0, 1))
    return img


def save(img, path):
    Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).save(path, quality=74, optimize=True, progressive=False)


out = sys.argv[1]
save(sky(False), f"{out}/sky-light.jpg")
save(sky(True), f"{out}/sky-dark.jpg")
save(forest(False), f"{out}/forest-light.jpg")
save(forest(True), f"{out}/forest-dark.jpg")
