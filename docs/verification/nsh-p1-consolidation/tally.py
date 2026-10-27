"""Combine M5 baseline and W1/W2/W3 worker dispositions per acceptance item."""
import json, subprocess, collections, sys
def load(ref, path): return json.loads(subprocess.check_output(['git', 'show', f'{ref}:{path}']))
m5 = load('HEAD', 'docs/nsh-phase1-m5-results.json')
w1 = load('nsh-p1-fix-workspace', 'docs/nsh-phase1-w1-results.json')['acceptance']['sections_5_5_to_5_8']
w2 = {i['id']: i['status'] for i in load('nsh-p1-fix-host', 'docs/nsh-phase1-w2-host-results.json')['items']} if isinstance(load('nsh-p1-fix-host', 'docs/nsh-phase1-w2-host-results.json')['items'], list) else None
w2raw = load('nsh-p1-fix-host', 'docs/nsh-phase1-w2-host-results.json')['items']
if w2 is None:
    w2 = {k: (v['w2'] if isinstance(v, dict) and 'w2' in v else v.get('status') if isinstance(v, dict) else v) for k, v in w2raw.items()}
w3 = {i['id']: i['status'] for i in load('nsh-p1-fix-native', 'docs/nsh-phase1-w3-native-results.json')['items']}
rows = []
for item, data in m5['items'].items():
    base = data.get('result') or data.get('status') or data.get('disposition')
    owner, latest = 'M5', base
    for name, table in (('W1', w1), ('W2', w2), ('W3', w3)):
        if item in table:
            owner, latest = name, table[item]
    rows.append((data['section'], item, base, owner, latest))
json.dump(rows, open(sys.argv[1], 'w'), indent=1)
by = collections.OrderedDict()
for section, item, base, owner, latest in rows:
    by.setdefault(section, collections.Counter())[latest] += 1
for section, counts in by.items(): print(f'{section:70s} {dict(counts)}')
total = collections.Counter(r[4] for r in rows); print('TOTAL', dict(total))
print('owners', collections.Counter(r[3] for r in rows))
