"""Checks the Python bindings against files made by ffmpeg and read back by ffprobe.

Run with `python -m unittest discover -s tpt-kinetix-py/tests` after installing the
package (`maturin develop`), or let `tools/py-binding-test.sh` do both.
"""

import json
import os
import subprocess
import tempfile
import unittest

import tpt_kinetix

KEY_ID = "00112233445566778899aabbccddeeff"
KEY = "ffeeddccbbaa99887766554433221100"


def make_media(directory):
    """An H.264 + AAC MP4 and a VP9 + Opus WebM, four seconds each."""
    mp4 = os.path.join(directory, "av.mp4")
    webm = os.path.join(directory, "av.webm")
    video = ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"]
    audio = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"]
    subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", *video, *audio, "-t", "4",
         "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", mp4],
        check=True,
    )
    subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", *video, *audio, "-t", "4",
         "-c:v", "libvpx-vp9", "-b:v", "300k", "-pix_fmt", "yuv420p",
         "-c:a", "libopus", "-ac", "2", webm],
        check=True,
    )
    return mp4, webm


def ffprobe_codecs(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=codec_name",
         "-of", "json", path],
        check=True, capture_output=True, text=True,
    ).stdout
    return [s["codec_name"] for s in json.loads(out)["streams"]]


class BindingsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.mkdtemp(prefix="tpt-py-")
        cls.mp4, cls.webm = make_media(cls.dir)

    def test_version(self):
        self.assertRegex(tpt_kinetix.version(), r"^\d+\.\d+\.\d+$")

    def test_probe_reports_the_tracks(self):
        info = tpt_kinetix.probe(self.mp4)
        self.assertTrue(info["format"].startswith("mov"))
        self.assertEqual([s["codec"] for s in info["streams"]], ["h264", "aac"])
        self.assertEqual(info["streams"][0]["width"], 320)
        self.assertEqual(info["streams"][1]["channels"], 2)

    def test_probe_of_a_missing_file_raises(self):
        with self.assertRaises(tpt_kinetix.KinetixError):
            tpt_kinetix.probe(os.path.join(self.dir, "nope.mp4"))

    def test_faststart_and_remux_keep_the_tracks(self):
        fast = os.path.join(self.dir, "fast.mp4")
        tpt_kinetix.faststart(self.mp4, fast)
        self.assertEqual(ffprobe_codecs(fast), ["h264", "aac"])

        remuxed = os.path.join(self.dir, "remux.webm")
        tpt_kinetix.remux(self.webm, remuxed)
        self.assertEqual(ffprobe_codecs(remuxed), ["vp9", "opus"])

        frag = os.path.join(self.dir, "frag.mp4")
        tpt_kinetix.remux(self.mp4, frag, fragment_ms=1000)
        self.assertEqual(ffprobe_codecs(frag), ["h264", "aac"])

    def test_package_writes_hls_and_dash(self):
        out = os.path.join(self.dir, "pkg")
        counts = tpt_kinetix.package(self.mp4, out, segment_seconds=2.0)
        self.assertEqual(counts["tracks"], 2)
        self.assertGreater(counts["segments"], 0)
        for name in ("master.m3u8", "manifest.mpd", "track-0.m3u8", "init-0.mp4"):
            self.assertTrue(os.path.isfile(os.path.join(out, name)), name)
        self.assertFalse(os.path.exists(os.path.join(out, "key.json")))

    def test_cenc_package_writes_a_key_file(self):
        out = os.path.join(self.dir, "pkg-cenc")
        tpt_kinetix.package(self.webm, out, segment_seconds=2.0,
                            key_id=KEY_ID, key=KEY, scheme="cenc")
        with open(os.path.join(out, "key.json")) as f:
            self.assertIn("keys", json.load(f))

    def test_half_a_key_raises(self):
        with self.assertRaises(tpt_kinetix.KinetixError):
            tpt_kinetix.package(self.mp4, os.path.join(self.dir, "x"), key_id=KEY_ID)


if __name__ == "__main__":
    unittest.main(verbosity=2)
