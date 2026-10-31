#!/usr/bin/env python3
"""Check the files Git would publish for local-only inputs and stale source paths."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
# The independently published client never contains a signing private key.
RELEASE_KEY = None
PRIVATE_KEY = re.compile(r'-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----')
STALE_PATH = re.compile(
    r'(?:crates/(?:bluetooth-audio-(?:core|device)|lyra-player-core)|'
    r'cli[/]canopus[/]|tools[/]canopus-core[/]|watchfaces[/]canopus-installer)'
)
PERSONAL_PATH = re.compile(
    r'(?:[CD]:[/\\](?:Users[/\\]angel|Canopus-main|Chaos-Module|LuaDevTemplate)|'
    r'/[cd]/(?:Users/angel|Canopus-main|Chaos-Module|LuaDevTemplate))', re.I
)


def main():
    result = subprocess.run(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
        cwd=ROOT, check=True, capture_output=True,
    )
    paths = sorted(set(result.stdout.decode('utf-8').rstrip('\0').split('\0')) - {''})
    failures = []
    for name in paths:
        path = ROOT / name
        parts = Path(name).parts
        if (parts[0] in {'out', '.canopus-local', '_band9_pack'} and name != RELEASE_KEY) or path.name == 'build.local.toml':
            failures.append(name + ': local file is tracked')
        if name == '.cargo/config.toml' and (ROOT / 'scripts/configure.py').exists():
            failures.append(name + ': generated SDK configuration is tracked')
        if not path.is_file() or path.stat().st_size > 4 * 1024 * 1024:
            continue
        text = path.read_bytes().decode('utf-8', errors='replace')
        if PRIVATE_KEY.search(text) and name != RELEASE_KEY:
            failures.append(name + ': private-key content')
        # Evidence and attribution may name original projects, but executable
        # source must not resolve obsolete paths or a maintainer's checkout.
        if path.suffix in {'.rs', '.c', '.h', '.lua', '.py', '.ps1', '.sh', '.toml', '.yml'}:
            if parts[0] != 'docs' and STALE_PATH.search(text):
                failures.append(name + ': obsolete source path')
            if PERSONAL_PATH.search(text):
                failures.append(name + ': maintainer-specific path')
    if failures:
        raise SystemExit('\n'.join(failures))
    print(f'PASS: {len(paths)} publishable files; no undeclared private keys, local inputs or obsolete source paths.')


if __name__ == '__main__':
    main()
