"""Build ChemClient's AstroBox distribution; optionally produce a local ABP."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parent
DIST = ROOT / 'dist'
BUILD = ROOT / 'out/cargo'

def component_linker(explicit=None):
    """API 4 async WIT requires a recent componentizer, independently of rustc."""
    candidates = [explicit, os.environ.get('CARGO_TARGET_WASM32_WASIP2_LINKER'),
                  shutil.which('wasm-component-ld')]
    for command in [['rustc', '--print', 'sysroot'],
                    ['rustup', 'run', 'nightly', 'rustc', '--print', 'sysroot']]:
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode == 0:
            root = Path(result.stdout.strip()) / 'lib/rustlib'
            candidates.extend(str(p) for p in root.glob('*/bin/wasm-component-ld*') if p.is_file())
    for candidate in candidates:
        if not candidate or not Path(candidate).is_file():
            continue
        try:
            result = subprocess.run([candidate, '--version'], capture_output=True, text=True)
        except OSError:
            continue
        version = re.search(r'wasm-component-ld (\d+)\.(\d+)\.(\d+)', result.stdout)
        if version and tuple(map(int, version.groups())) >= (0, 5, 27):
            return str(Path(candidate).resolve())
        if explicit:
            break
    raise RuntimeError('API 4 needs wasm-component-ld >= 0.5.27. Install it or pass --component-linker PATH.')

def validate(directory):
    manifest = json.loads((directory / 'manifest.json').read_text(encoding='utf-8'))
    for key in ['name','icon','version','description','author','website','entry']:
        if not isinstance(manifest.get(key),str):raise ValueError('Missing string '+key)
    for key in ['entry','icon']:
        name=manifest[key]
        if Path(name).name != name or not (directory/name).is_file():
            raise ValueError('Missing/unsafe '+key)
    if manifest['api_level'] != 4 or manifest['wasi_version'] != 2:
        raise ValueError('Expected API 4 / WASI 2')
    wasm=(directory/manifest['entry']).read_bytes()
    if wasm[:8] != b'\0asm\x0d\0\x01\0':raise ValueError('Entry is not a WASM component')
    for interface, callback in [('lifecycle', 'on-load'), ('event', 'on-event'),
                                ('event', 'on-ui-event'), ('event', 'on-ui-render'),
                                ('event', 'on-card-render')]:
        symbol = ('[callback][async-lift]astrobox:psys-plugin-v4/' + interface + '#' + callback).encode()
        if symbol not in wasm:
            raise ValueError('Missing async executor callback: ' + callback)
    if (directory/manifest['icon']).read_bytes()[:8] != b'\x89PNG\r\n\x1a\n':raise ValueError('Invalid PNG')
    for name in manifest['additional_files']:
        target=(directory/name).resolve()
        if directory.resolve() not in target.parents or not target.is_file():raise ValueError('Missing additional file '+name)
    return manifest

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--abp','--package',dest='abp',action='store_true',
                        help='Also create dist/ChemClient.abp for local installation')
    parser.add_argument('--check',action='store_true')
    parser.add_argument('--website',help='Actual public project/repository URL')
    parser.add_argument('--component-linker', help='Path to wasm-component-ld >= 0.5.27')
    args=parser.parse_args()
    if not args.check:
        env = os.environ.copy()
        env['CARGO_TARGET_DIR'] = str(BUILD)
        env['CARGO_TARGET_WASM32_WASIP2_LINKER'] = component_linker(args.component_linker)
        print('Component linker:', env['CARGO_TARGET_WASM32_WASIP2_LINKER'], flush=True)
        subprocess.run(['cargo','build','--release','--locked'],cwd=ROOT,check=True,env=env)
        DIST.mkdir(parents=True,exist_ok=True)
        manifest=json.loads((ROOT/'manifest.json').read_text(encoding='utf-8'))
        for name in [manifest['icon'], *manifest['additional_files']]:
            source=(ROOT/name).resolve()
            destination=(DIST/name).resolve()
            if ROOT not in source.parents or DIST not in destination.parents:
                raise ValueError('Unsafe resource path '+name)
            destination.parent.mkdir(parents=True,exist_ok=True)
            shutil.copy2(source,destination)
        if Path(manifest['entry']).name != manifest['entry']:
            raise ValueError('Unsafe entry path')
        shutil.copy2(BUILD/'wasm32-wasip2/release/chemclient.wasm',DIST/manifest['entry'])
        if args.website:
            manifest['website']=args.website
        (DIST/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    manifest=validate(DIST)
    folders=[line.strip() for line in (ROOT/'index.txt').read_text(encoding='utf-8').splitlines()
             if line.strip() and not line.lstrip().startswith('#')]
    if DIST.relative_to(ROOT).as_posix() not in folders:
        raise ValueError('Plugin repository index.txt must include dist')
    print('PASS: API 4 / WASI 2 component, manifest, PNG and resource layout')
    if not manifest['website']:print('Publication pending: supply the real repository URL with --website')
    if args.abp:
        with zipfile.ZipFile(DIST/'ChemClient.abp','w',compression=zipfile.ZIP_DEFLATED) as archive:
            for name in ['manifest.json',manifest['entry'],manifest['icon'],*manifest['additional_files']]:
                archive.write(DIST/name,name)
        print('Package:', DIST/'ChemClient.abp')
    print('Distribution:', DIST)
    for name in ['manifest.json',manifest['entry'],manifest['icon']]:
        print(name,hashlib.sha256((DIST/name).read_bytes()).hexdigest())

if __name__=='__main__':main()
