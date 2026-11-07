"""Probe, remux and package media files with the TPT Kinetix engine.

Everything runs in-process and nothing is decoded: the functions work on the
container. Failures raise :class:`KinetixError`.

>>> import tpt_kinetix
>>> tpt_kinetix.probe("clip.webm")["streams"][0]["codec"]
'vp9'
"""

from ._tpt_kinetix import KinetixError, faststart, package, probe, remux, version

__all__ = ["KinetixError", "faststart", "package", "probe", "remux", "version"]
