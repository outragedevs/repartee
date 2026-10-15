import argparse
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN = {
    "proc-macro-error": "RUSTSEC-2024-0370",
    "proc-macro-error-attr": "helper for the unmaintained proc-macro-error",
    "proc-macro-error2": "RUSTSEC-2026-0173",
    "proc-macro-error-attr2": "helper for the unmaintained proc-macro-error2",
}


def check(manifest):
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
         "--manifest-path", str(manifest)],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    packages = json.loads(result.stdout)["packages"]
    if not packages:
        raise ValueError("Cargo returned an empty dependency graph")
    violations = []
    for package in packages:
        name = package["name"]
        if name in FORBIDDEN:
            violations.append(f"{name} {package['version']}: {FORBIDDEN[name]}")
    return sorted(violations)


def main():
    parser = argparse.ArgumentParser(description="Reject retired macro dependencies across the Cargo workspace.")
    parser.add_argument("--manifest-path", type=Path, default=ROOT / "Cargo.toml")
    args = parser.parse_args()
    try:
        violations = check(args.manifest_path)
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError, TypeError) as error:
        print(f"Dependency policy could not be checked: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 1
    if violations:
        print("Dependency policy failed:\n" + "\n".join(violations), file=sys.stderr)
        return 1
    print("Dependency policy passed: retired macro packages are absent (all workspace features and targets).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
