import pathlib
import re
import sys

ESCAPES = re.compile(r'"/\.\./')
offenders = []

for path in pathlib.Path("crates").rglob("*.rs"):
    lines = path.read_text().splitlines()
    tests_start = next(
        (n for n, line in enumerate(lines) if line.strip() == "#[cfg(test)]"),
        len(lines),
    )
    for n, line in enumerate(lines):
        if n < tests_start and ESCAPES.search(line):
            offenders.append(f"{path}:{n + 1}: {line.strip()}")

if not offenders:
    print("every include outside test code stays inside its crate")
    sys.exit(0)

print("a crate reaches outside its own directory, which crates.io cannot package:")
print("\n".join(offenders))
print("\nmove the file under the crate and pin the copies with a test")
sys.exit(1)
