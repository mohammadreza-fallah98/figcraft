#!/usr/bin/env bash
# App-specific checks retained alongside shared packaging lint.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

bash packaging/server/install-test.sh
if grep -rniE 'photoshop|psd|photocraft|vectorcraft|illustrator|adobe|figma' packaging/ .github/ \
    --exclude=packaging-lint.yml --exclude=check-app-specific.sh; then
  echo "::error::package metadata mentions another product (see above)" >&2
  exit 1
fi

python3 - <<'PYEOF'
import yaml
manifests = []
for name in ("ai.figiran.app.yml", "ai.figiran.app.bundle.yml"):
    with open("packaging/linux/flatpak/" + name) as source:
        manifest = yaml.safe_load(source)
    assert manifest["id"] == "ai.figiran.app" and manifest["command"] == "figiran", name
    manifests.append(manifest)
for key in ("runtime", "runtime-version", "sdk", "finish-args"):
    assert manifests[0][key] == manifests[1][key], f"Flatpak manifests disagree on {key}"
print("FigIran package identity ok")
PYEOF
