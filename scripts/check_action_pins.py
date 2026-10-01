#!/usr/bin/env python3
# Copyright 2026 PackDb Contributors
#
# Licensed under the Apache License, Version 2.0
"""Fail when a workflow action pin moves or trails its latest stable release.

Scans this repository's own `.github/workflows`. Every remote action must be
pinned to a full commit SHA with its exact release on the same line --
`uses: owner/action@<40-hex> # vX.Y.Z` -- the form Dependabot maintains. The
live check resolves each pinned tag to its commit and compares the pin with the
action's latest stable release. An empty scan is a failure, never a pass.

    check_action_pins.py              # live check (reads GITHUB_TOKEN if set)
    check_action_pins.py --self-test  # offline controls, no network
"""

import json
import os
from pathlib import Path
import re
import sys
import tempfile
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
USE = re.compile(r'^\s*-?\s*uses:\s+([\w.-]+/[\w.-]+)@([0-9a-f]{40})\s+#\s+(v\d+\.\d+\.\d+)\s*$')
REMOTE = re.compile(r'^\s*-?\s*uses:\s+([^\s#]+)')
VERSION = re.compile(r'^v(\d+)\.(\d+)\.(\d+)$')


def pins(root=ROOT):
    workflows = root / '.github' / 'workflows'
    files = sorted(list(workflows.glob('*.yml')) + list(workflows.glob('*.yaml')))
    if not files:
        raise ValueError(f'no workflow files under {workflows}')
    found = {}
    for path in files:
        for number, line in enumerate(path.read_text().splitlines(), 1):
            use = REMOTE.match(line)
            if not use or use.group(1).startswith('./') or use.group(1).startswith('docker://'):
                continue
            match = USE.match(line)
            if not match:
                raise ValueError(f'{path.name}:{number}: remote action needs a full SHA and an exact vX.Y.Z comment')
            name, sha, version = match.groups()
            if name in found and found[name] != (sha, version):
                raise ValueError(f'{path.name}:{number}: conflicting pin for {name}: {found[name]} versus {(sha, version)}')
            found[name] = (sha, version)
    if not found:
        raise ValueError('no action pins examined')
    return found


def github_json(path):
    headers = {'Accept': 'application/vnd.github+json', 'User-Agent': 'packdb-action-pin-watch'}
    token = os.environ.get('GITHUB_TOKEN')
    if token:
        headers['Authorization'] = f'Bearer {token}'
    with urlopen(Request(f'https://api.github.com/{path}', headers=headers), timeout=20) as response:
        return json.load(response)


def tag_commit(name, tag, get=github_json):
    obj = get(f'repos/{name}/git/ref/tags/{tag}')['object']
    for _ in range(5):
        if obj['type'] == 'commit':
            return obj['sha']
        if obj['type'] != 'tag':
            raise ValueError(f'{name}@{tag}: unexpected ref type {obj["type"]}')
        obj = get(f'repos/{name}/git/tags/{obj["sha"]}')['object']
    raise ValueError(f'{name}@{tag}: annotated tag chain too deep')


def check(found, get=github_json):
    failures = []
    for name, (sha, version) in sorted(found.items()):
        try:
            actual = tag_commit(name, version, get)
            latest = get(f'repos/{name}/releases/latest')['tag_name']
            if not VERSION.fullmatch(latest):
                raise ValueError(f'latest release tag is not vX.Y.Z: {latest}')
            pinned = tuple(map(int, VERSION.fullmatch(version).groups()))
            newest = tuple(map(int, VERSION.fullmatch(latest).groups()))
            if actual != sha:
                failures.append(f'{name}: {version} resolves to {actual}, pinned {sha}')
            if pinned < newest:
                failures.append(f'{name}: {version} trails latest stable {latest}')
            elif pinned > newest:
                failures.append(f'{name}: {version} is ahead of latest stable {latest}; review release metadata')
            if actual == sha and version == latest:
                print(f'PASS {name}@{version} {sha}')
        except (KeyError, ValueError, HTTPError, URLError) as error:
            failures.append(f'{name}: lookup failed: {error}')
    return failures


def self_test():
    sha, other = 'a' * 40, 'b' * 40

    def releases(latest, ref_object):
        def get(path):
            if path.endswith('/releases/latest'):
                return {'tag_name': latest}
            if '/git/ref/' in path:
                return {'object': ref_object}
            return {'object': {'type': 'commit', 'sha': sha}}
        return get

    def refuses(text, pattern):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if text is not None:
                (root / '.github' / 'workflows').mkdir(parents=True)
                (root / '.github' / 'workflows' / 'ci.yml').write_text(text)
            try:
                pins(root)
            except ValueError as error:
                assert re.search(pattern, str(error)), error
                return
            raise AssertionError(f'accepted: {text!r}')

    refuses(None, 'no workflow files')
    refuses('jobs: {}\n', 'no action pins examined')
    refuses('      - uses: actions/checkout@v4\n', 'full SHA and an exact')
    refuses(f'      - uses: actions/checkout@{sha} # v4\n', 'full SHA and an exact')
    refuses(f'      - uses: a/b@{sha} # v1.0.0\n      - uses: a/b@{other} # v1.0.1\n', 'conflicting pin')
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / '.github' / 'workflows').mkdir(parents=True)
        (root / '.github' / 'workflows' / 'ci.yml').write_text(
            f'      - uses: ./local\n      - uses: a/b@{sha} # v1.0.0\n')
        assert pins(root) == {'a/b': (sha, 'v1.0.0')}
    moved = check({'a/b': (sha, 'v1.0.0')}, releases('v1.1.0', {'type': 'commit', 'sha': other}))
    assert len(moved) == 2 and 'resolves to' in moved[0] and 'trails latest' in moved[1], moved
    assert check({'a/b': (sha, 'v1.0.0')}, releases('v1.0.0', {'type': 'tag', 'sha': other})) == []
    ahead = check({'a/b': (sha, 'v2.0.0')}, releases('v1.0.0', {'type': 'commit', 'sha': sha}))
    assert len(ahead) == 1 and 'ahead of latest' in ahead[0], ahead

    def unreachable(path):
        raise URLError('offline')
    assert 'lookup failed' in check({'a/b': (sha, 'v1.0.0')}, unreachable)[0]
    print('check_action_pins self-test: 10 controls passed')
    return 0


def main(argv):
    if argv[1:] == ['--self-test']:
        return self_test()
    try:
        failures = check(pins())
    except (ValueError, OSError) as error:
        failures = [str(error)]
    for failure in failures:
        print(f'FAIL {failure}', file=sys.stderr)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
