"""Compare JM's temporal-direct MB coverage against our KCOL119 probe data."""
import os, re, tempfile
TMP = tempfile.gettempdir().replace(chr(92), "/")
jm = {}
with open(TMP + '/jm_bin/jmt4.log', encoding='utf-8', errors='replace') as f:
    for line in f:
        m = re.match(r'JMT mbx=(\d+) mby=(\d+) m=(\d+),(\d+),(\d+),(\d+) p=(\d+),(\d+),(\d+),(\d+)', line.strip())
        if m:
            mbx, mby = int(m.group(1)), int(m.group(2))
            jm[(mbx, mby)] = tuple(int(m.group(i)) for i in range(3, 11))
ours = {}
with open(TMP + '/runk61.log', encoding='utf-8', errors='replace') as f:
    for line in f:
        m = re.match(r'KCOL119 mbx=(\d+) mby=(\d+) j0=(\d+) i0=(\d+) ', line.strip())
        if m:
            mbx, mby, j0, i0 = int(m.group(1)), int(m.group(2)), int(m.group(3)), int(m.group(4))
            key = (mbx, mby)
            q = (0 if j0 == 0 else 2) + (1 if (i0 % 4) >= 2 else 0)
            ours.setdefault(key, set()).add(q)
jmd = {k: tuple(1 if v[i] == 0 else 0 for i in range(4)) for k, v in jm.items()}
alld = sorted(set(jmd) | set(ours))
print('JM temporal-called MBs:', len(jmd), '| our direct-read MBs:', len(ours))
mismatch = 0
for k in alld:
    jmq = jmd.get(k)
    oq = ours.get(k)
    if jmq is None:
        print('MB(%d,%d): JM not-called, ours quads=%s' % (k[0], k[1], sorted(oq)))
        mismatch += 1
    else:
        jq = set(i for i in range(4) if jmq[i])
        oq = oq if oq is not None else set()
        if jq != oq:
            print('MB(%d,%d): JM direct quads=%s ours=%s' % (k[0], k[1], sorted(jq), sorted(oq)))
            mismatch += 1
print('mismatches:', mismatch)
