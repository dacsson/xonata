#!/usr/bin/env bash
set -euo pipefail

bash scripts/build-web.sh

# Stage only distributable browser assets, independently of other repository files.
python3 - <<'PY'
from pathlib import Path
import shutil

source = Path('web')
destination = Path('target/pages')
if destination.is_symlink():
    raise SystemExit('Refusing to replace a symlink at target/pages')
if destination.exists():
    shutil.rmtree(destination)
destination.mkdir(parents=True)
for name in ['index.html', 'embed-example.html', 'worker.js', 'font-license.txt']:
    shutil.copy2(source / name, destination / name)
shutil.copytree(source / 'pkg', destination / 'pkg')
(destination / '.nojekyll').touch()
print(f'GitHub Pages artifact: {destination}')
PY
