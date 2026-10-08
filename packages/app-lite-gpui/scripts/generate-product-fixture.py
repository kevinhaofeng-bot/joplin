#!/usr/bin/env python3
"""Generate synthetic ENEX input for the ordinary notes product, not a spike.

No repository/API/database writes. Output is a new, private directory; existing
destinations are refused. PNG rows and resource base64 are streamed. Random RGB
pixels avoid measuring ten references to a single tiny, easily cached image.
The manifest describes input only: it never marks import/performance accepted.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import random
import struct
from xml.sax.saxutils import escape
import zlib


def _png_chunk(stream, kind, payload):
    stream.write(struct.pack(">I", len(payload)))
    stream.write(kind)
    stream.write(payload)
    stream.write(struct.pack(">I", zlib.crc32(kind + payload)))


def _write_png(path, width, height, seed):
    pixels = random.Random(seed)
    compressor = zlib.compressobj(1)
    with path.open("xb") as stream:
        stream.write(b"\x89PNG\r\n\x1a\n")
        _png_chunk(stream, b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        for _ in range(height):
            compressed = compressor.compress(b"\0" + pixels.randbytes(width * 3))
            if compressed:
                _png_chunk(stream, b"IDAT", compressed)
        _png_chunk(stream, b"IDAT", compressor.flush())
        _png_chunk(stream, b"IEND", b"")


def _asset(path, mime, width=0, height=0):
    md5, sha = hashlib.md5(), hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(64 * 1024), b""):
            md5.update(chunk)
            sha.update(chunk)
    return {"filename": path.name, "mime": mime, "md5": md5.hexdigest(),
            "sha256": sha.hexdigest(), "size": path.stat().st_size,
            "width": width, "height": height}


def _media(asset):
    return '<div><en-media type="{}" hash="{}" /></div>'.format(asset["mime"], asset["md5"])


def _write_resource(stream, asset, assets_dir):
    stream.write(b'<resource><data encoding="base64">')
    with (assets_dir / asset["filename"]).open("rb") as source:
        # A multiple of three keeps concatenated base64 chunks valid.
        for chunk in iter(lambda: source.read(48 * 1024), b""):
            stream.write(base64.b64encode(chunk))
    stream.write(('</data><mime>{}</mime><width>{}</width><height>{}</height>'
                  '<resource-attributes><file-name>{}</file-name></resource-attributes></resource>'
                  .format(asset["mime"], asset["width"], asset["height"], escape(asset["filename"]))).encode())


def generate_fixture(output, *, notes=1662, resources=4238, blocks=200,
                     images=10, card_notes=50, image_width=2000, image_height=1500):
    values = (notes, resources, blocks, images, card_notes, image_width, image_height)
    if any(type(value) is not int for value in values):
        raise ValueError("fixture dimensions and counts must be integers")
    if not (2 <= notes <= 50000 and 1 <= images <= 100 and
            2 * images + 1 <= blocks <= 10000 and 0 <= card_notes < notes and
            images + card_notes <= resources <= 50000 and
            1 <= image_width <= 4096 and 1 <= image_height <= 4096):
        raise ValueError("invalid bounded product fixture configuration")
    output = Path(output)
    if not output.is_absolute() or not output.parent.is_dir() or output.parent.is_symlink():
        raise ValueError("output requires a real existing parent and an absolute new path")
    # This atomic creation must fail before touching any existing contents.
    output.mkdir(mode=0o700)
    assets_dir = output / "assets"
    assets_dir.mkdir(mode=0o700)
    hot_assets, card_assets, all_assets = [], [], []
    for index in range(images):
        path = assets_dir / f"hot-{index:02}.png"
        _write_png(path, image_width, image_height, 1000 + index)
        hot_assets.append(_asset(path, "image/png", image_width, image_height))
    for index in range(card_notes):
        path = assets_dir / f"card-{index:04}.png"
        _write_png(path, 400, 300, 100000 + index)
        card_assets.append(_asset(path, "image/png", 400, 300))
    all_assets.extend(hot_assets)
    all_assets.extend(card_assets)
    text_count = resources - images - card_notes
    text_assets = []
    for index in range(text_count):
        path = assets_dir / f"attachment-{index:05}.txt"
        with path.open("xb") as stream:
            stream.write(f"整款负载257 附件独有词{index:05} 本地离线检索，原始内容需完整保留。\n".encode())
        text_assets.append(_asset(path, "text/plain"))
    all_assets.extend(text_assets)

    hot_title = f"整款负载257 · {blocks}块{images}图"
    paragraphs = blocks - images
    hot_body, emitted_images = [], 0
    for paragraph in range(1, paragraphs + 1):
        hot_body.append(f"<div>第{paragraph:03}段：中文图文输入、离线保存、搜索与撤销。"
                        f"Native product paragraph {paragraph:03}. "
                        "<strong>粗体</strong>与<em>斜体</em>保留。</div>")
        target = paragraph * images // (paragraphs - 1) if paragraph < paragraphs else images
        while emitted_images < target:
            hot_body.append(_media(hot_assets[emitted_images]))
            emitted_images += 1
    per_note, extra = divmod(text_count, notes - 1)
    text_offset = 0
    source = output / "product-fixture.enex"
    with source.open("xb") as stream:
        stream.write(b'<?xml version="1.0" encoding="UTF-8"?><en-export>')
        for note_index in range(notes):
            if note_index == 0:
                title, body, assets = hot_title, "".join(hot_body), hot_assets
            else:
                title = f"整款负载257 卡片 {note_index:04}"
                count = per_note + int(note_index <= extra)
                assets = text_assets[text_offset:text_offset + count]
                text_offset += count
                if note_index <= card_notes:
                    assets = [card_assets[note_index - 1]] + assets
                body = f"<div>第{note_index:04}篇合成笔记，包含中文、Latin、独立资源。" + ("可浏览、可检索、可恢复。" * 24) + "</div>"
                body += "".join(_media(asset) for asset in assets)
            updated = "20260901T120000Z" if note_index == 0 else "20260101T120000Z"
            stream.write((f"<note><title>{escape(title)}</title><content><![CDATA[<en-note>{body}</en-note>]]></content>"
                          f"<created>20260101T120000Z</created><updated>{updated}</updated>"
                          f"<tag>整款负载257</tag><tag>主题{note_index % 16:02}</tag>").encode())
            for asset in assets:
                _write_resource(stream, asset, assets_dir)
            stream.write(b"</note>")
        stream.write(b"</en-export>")
    assert text_offset == text_count
    manifest = {
        "schema_version": 1, "synthetic": True, "full_product_acceptance": False,
        "notes": notes, "resources": resources, "unique_blob_inputs": len(all_assets),
        "hot_title": hot_title, "hot_enml_blocks": blocks,
        "hot_text_blocks": paragraphs, "hot_image_blocks": images,
        "card_notes": card_notes, "image_width": image_width, "image_height": image_height,
        "source": source.name, "source_size": source.stat().st_size,
        "source_sha256": _asset(source, "application/xml")["sha256"],
        "assets": all_assets,
        "limits": "Input fixture only; actual imported blocks, 50 concurrent visible cards, memory and latency require runtime evidence.",
    }
    with (output / "fixture-manifest.json").open("x") as stream:
        json.dump(manifest, stream, ensure_ascii=False, indent=2)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    manifest = generate_fixture(args.output)
    print(json.dumps({k: v for k, v in manifest.items() if k != "assets"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
