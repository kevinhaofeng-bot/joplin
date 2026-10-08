"""Catch incomplete/corrupt fixture output, lost references, and overwrites.

This validates the synthetic input, not the notes product or its performance.
Native import and runtime observations remain separate acceptance gates.
"""
import base64
import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zlib


MODULE_PATH = Path(__file__).with_name("generate-product-fixture.py")
SPEC = importlib.util.spec_from_file_location("product_fixture", MODULE_PATH)
FIXTURE = importlib.util.module_from_spec(SPEC)
if MODULE_PATH.exists():
    SPEC.loader.exec_module(FIXTURE)


class ProductFixtureTests(unittest.TestCase):
    def generate(self, destination):
        self.assertTrue(hasattr(FIXTURE, "generate_fixture"), "missing bounded ENEX fixture generator")
        return FIXTURE.generate_fixture(
            destination, notes=4, resources=9, blocks=7, images=2,
            card_notes=2, image_width=24, image_height=16,
        )

    def test_enex_has_exact_notes_blocks_and_bound_resources(self):
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "new-fixture"
            manifest = self.generate(output)
            export = ET.parse(output / "product-fixture.enex").getroot()
            notes = export.findall("note")
            self.assertEqual(len(notes), 4)
            self.assertEqual(sum(len(n.findall("resource")) for n in notes), 9)
            hot = ET.fromstring(notes[0].findtext("content"))
            self.assertEqual(len(hot), 7)
            self.assertEqual(sum(len(b.findall("en-media")) for b in hot), 2)
            self.assertEqual(hot[0].tag, "div")
            self.assertIsNone(hot[-1].find("en-media"), "typing after the last image needs a real text block")
            all_sha = set()
            for note in notes:
                body = ET.fromstring(note.findtext("content"))
                hashes = set()
                for resource in note.findall("resource"):
                    data = base64.b64decode(resource.findtext("data"), validate=True)
                    name = resource.findtext("resource-attributes/file-name")
                    self.assertEqual(data, (output / "assets" / name).read_bytes())
                    hashes.add(hashlib.md5(data).hexdigest())
                    all_sha.add(hashlib.sha256(data).hexdigest())
                self.assertEqual({m.attrib["hash"] for m in body.iter("en-media")}, hashes)
            self.assertEqual(len(all_sha), 9, "distinct resources must not collapse to one easy cache entry")
            self.assertEqual(manifest["notes"], 4)
            self.assertEqual(manifest["resources"], 9)
            self.assertEqual(manifest["hot_text_blocks"], 5)
            self.assertEqual(manifest["hot_image_blocks"], 2)
            self.assertEqual(manifest["card_notes"], 2)

    def test_pngs_decode_with_valid_chunks_and_declared_dimensions(self):
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "new-fixture"
            self.generate(output)
            data = (output / "assets" / "hot-00.png").read_bytes()
            self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
            offset, compressed, kinds = 8, bytearray(), []
            while offset < len(data):
                length = struct.unpack(">I", data[offset:offset+4])[0]
                kind = data[offset+4:offset+8]
                payload = data[offset+8:offset+8+length]
                crc = struct.unpack(">I", data[offset+8+length:offset+12+length])[0]
                self.assertEqual(crc, zlib.crc32(kind + payload))
                if kind == b"IHDR":
                    self.assertEqual(struct.unpack(">IIBBBBB", payload), (24, 16, 8, 2, 0, 0, 0))
                elif kind == b"IDAT":
                    compressed.extend(payload)
                kinds.append(kind)
                offset += length + 12
            decoded = zlib.decompress(compressed)
            self.assertEqual(len(decoded), 1168)
            self.assertEqual([decoded[i * 73] for i in range(16)], [0] * 16)
            self.assertEqual(kinds[0], b"IHDR")
            self.assertEqual(kinds[-1], b"IEND")

    def test_regeneration_is_identical_and_existing_destination_is_untouched(self):
        with tempfile.TemporaryDirectory() as root:
            first, second = Path(root) / "one", Path(root) / "two"
            self.generate(first)
            self.generate(second)
            self.assertEqual((first / "product-fixture.enex").read_bytes(), (second / "product-fixture.enex").read_bytes())
            sentinel = first / "keep.txt"
            sentinel.write_bytes(b"must stay")
            with self.assertRaises(FileExistsError):
                self.generate(first)
            self.assertEqual(sentinel.read_bytes(), b"must stay")

    def test_invalid_configuration_creates_no_partial_fixture(self):
        self.assertTrue(hasattr(FIXTURE, "generate_fixture"), "missing bounded ENEX fixture generator")
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "invalid"
            with self.assertRaises(ValueError):
                FIXTURE.generate_fixture(output, notes=4, resources=1, blocks=7, images=2, card_notes=2)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
