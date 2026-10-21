#!/usr/bin/env python3
"""Measure Rust line coverage, optionally including the isolated Logseq imports.

Requires cargo, llvm-profdata, and llvm-cov matching rustc's LLVM version.
Uses no Python packages. Graphs are isolated by the Rust end-to-end tests.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import uuid


ROOT = Path(__file__).resolve().parents[1]


def run(args, env=None):
    result = subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f"{' '.join(map(str, args))}\n{result.stdout}\n{result.stderr}")
    return result.stdout, result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--e2e", action="store_true", help="Include both Logseq import tests")
    parser.add_argument("--output", type=Path, default=ROOT / "target/compatibility/coverage")
    args = parser.parse_args()
    output = args.output.resolve()
    # Each invocation has fresh counters, even when reusing compiled binaries.
    raw = output / "raw" / uuid.uuid4().hex
    raw.mkdir(parents=True)
    build = ROOT / "target/coverage-build"
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(build)
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C instrument-coverage").strip()
    env["LLVM_PROFILE_FILE"] = str(raw / "%p-%m.profraw")
    command = ["cargo", "test", "--locked", "--all-targets", "--message-format=json"]
    if args.e2e:
        command += ["--", "--include-ignored", "--test-threads=1"]
    stdout, stderr = run(command, env)
    (output / "tests.log").write_text(stdout + stderr)
    objects = set()
    for line in stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            objects.add(message["executable"])
    if not objects:
        raise RuntimeError("Cargo did not report any instrumented executables")

    # Also exercise the CLI's file-export path, which the library tests bypass.
    stdout, stderr = run([
        str(build / "debug/obsidian-to-logseq"), str(ROOT / "tests/fixtures/sample-vault"),
        "--output", str(output / "fixture.edn"),
    ], env)
    (output / "cli-export.log").write_text(stdout + stderr)
    profile = output / "coverage.profdata"
    profiles = sorted(raw.glob("*.profraw"))
    if not profiles:
        raise RuntimeError("No LLVM profiles were written")
    run(["llvm-profdata", "merge", "-sparse", *map(str, profiles), "-o", str(profile)])
    sources = sorted((ROOT / "src").glob("*.rs"))
    common = [f"--instr-profile={profile}"]
    for obj in sorted(objects):
        common += ["--object", obj]
    common += ["--sources", *map(str, sources)]
    report, _ = run(["llvm-cov", "report", *common])
    (output / "llvm-summary.txt").write_text(report)
    lcov, _ = run(["llvm-cov", "export", "--format=lcov", *common])
    (output / "coverage.lcov").write_text(lcov)
    run(["llvm-cov", "show", "--format=html", f"--output-dir={output / 'html'}", *common])

    # Exclude embedded #[cfg(test)] modules as well as standalone tests/examples
    # from the production line totals. All current modules put tests at the end.
    cutoffs = {}
    for source in sources:
        lines = source.read_text().splitlines()
        cutoffs[str(source)] = next((i for i, line in enumerate(lines, 1)
                                    if line.strip() == "#[cfg(test)]"), len(lines) + 1)
    counts = {}
    current = None
    for line in lcov.splitlines():
        if line.startswith("SF:"):
            current = str(Path(line[3:]).resolve())
        elif line.startswith("DA:") and current in cutoffs:
            number, hits, *_ = line[3:].split(",")
            if int(number) < cutoffs[current]:
                counts.setdefault(current, {})[int(number)] = int(hits) > 0
    files = []
    for name, lines in sorted(counts.items()):
        covered = sum(lines.values())
        files.append({"file": str(Path(name).relative_to(ROOT)), "covered": covered,
                      "lines": len(lines), "percent": round(100 * covered / len(lines), 2)})
    total = sum(f["lines"] for f in files)
    covered = sum(f["covered"] for f in files)
    summary = {"includes_e2e": args.e2e, "includes_cli_export": True,
               "scope": "src/*.rs, excluding embedded test modules; line coverage, not branch or format coverage",
               "covered": covered, "lines": total, "percent": round(100 * covered / total, 2),
               "files": files}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    print(f"HTML: {output / 'html/index.html'}")


if __name__ == "__main__":
    main()
