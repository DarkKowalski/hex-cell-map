"""Regression checks for portable native release archives."""
import os
from pathlib import Path
import tempfile
import unittest
import zipfile

from package import write_zip


class ArchiveTests(unittest.TestCase):
    def test_dependency_notice_with_old_timestamp_survives_archive_extraction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "hex-cell-map-windows-x64"
            notice = package / "dependency-notices" / "LICENSE"
            notice.parent.mkdir(parents=True)
            notice.write_text("Dependency notice / 依赖许可证\n", encoding="utf-8")
            os.utime(notice, (0, 0))
            archive = root / "package.zip"

            write_zip(package, archive)

            entry = "hex-cell-map-windows-x64/dependency-notices/LICENSE"
            with zipfile.ZipFile(archive) as stream:
                self.assertEqual(stream.getinfo(entry).date_time, (1980, 1, 1, 0, 0, 0))
                self.assertEqual(stream.read(entry), notice.read_bytes())
                stream.extractall(root / "extracted")
            self.assertEqual((root / "extracted" / entry).read_bytes(), notice.read_bytes())


if __name__ == "__main__":
    unittest.main()
