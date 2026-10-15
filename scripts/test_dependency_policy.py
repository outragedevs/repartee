from pathlib import Path
import subprocess
import tempfile
import unittest

from check_dependency_policy import FORBIDDEN, check


class DependencyPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dependency-policy-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "src").mkdir()
        (self.root / "src/lib.rs").write_text("")

    def manifest(self, extra=""):
        path = self.root / "Cargo.toml"
        path.write_text('[package]\nname = "policy-fixture"\nversion = "0.1.0"\nedition = "2024"\n' + extra)
        return path

    def lock(self, manifest):
        subprocess.run(
            ["cargo", "generate-lockfile", "--offline", "--manifest-path", str(manifest)],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def test_clean_workspace_passes(self):
        manifest = self.manifest()
        self.lock(manifest)
        self.assertEqual(check(manifest), [])

    def test_renamed_optional_target_specific_packages_are_rejected(self):
        for name in FORBIDDEN:
            with self.subTest(package=name):
                dependency = self.root / "retired"
                dependency.mkdir(exist_ok=True)
                (dependency / "src").mkdir(exist_ok=True)
                (dependency / "src/lib.rs").write_text("")
                (dependency / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "2.0.1"\nedition = "2024"\n'
                )
                manifest = self.manifest(
                    '[target.\'cfg(target_os = "none")\'.dependencies]\n'
                    f'innocent_alias = {{ package = "{name}", path = "retired", optional = true }}\n'
                )
                self.lock(manifest)
                self.assertEqual(check(manifest), [f"{name} 2.0.1: {FORBIDDEN[name]}"])

    def test_stale_lockfile_fails_closed(self):
        manifest = self.manifest()
        self.lock(manifest)
        manifest.write_text(manifest.read_text().replace('version = "0.1.0"', 'version = "0.2.0"'))
        with self.assertRaises(subprocess.CalledProcessError):
            check(manifest)


if __name__ == "__main__":
    unittest.main()
