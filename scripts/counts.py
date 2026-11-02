# Compares two phases of a GUNMETAL_COUNT run: guest instructions and calls per picture by function.
# usage: python scripts/counts.py <counts.txt> <frames file> <min imports per picture for A> <max for B>
import collections, sys
counts, frames, amin, bmax = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
pics = collections.Counter(); imp = collections.Counter()
for l in open(frames):
    if l.startswith('#'): continue
    p = l.split()
    if p[6] != '1': continue
    s = int(float(p[0]) / 1000) + 1; pics[s] += 1; imp[s] += int(p[9])
per = collections.defaultdict(dict)
for l in open(counts):
    p = l.split()
    per[int(p[0])][p[1]] = (int(p[2]), int(p[3]), ' '.join(p[4:]))
A = [s for s in pics if pics[s] > 30 and imp[s] / pics[s] >= amin]
B = [s for s in pics if pics[s] > 30 and imp[s] / pics[s] <= bmax]
def avg(secs):
    t = collections.defaultdict(lambda: [0.0, 0.0, '']); 
    for s in secs:
        for a, (n, i, name) in per[s].items():
            e = t[a]; e[0] += n / pics[s] / len(secs); e[1] += i / pics[s] / len(secs); e[2] = name
    return t
a, b = avg(A), avg(B)
ta = sum(e[1] for e in a.values()); tb = sum(e[1] for e in b.values())
print(f"A: {len(A)} s, {ta/1e6:.2f} M insns/picture;  B: {len(B)} s, {tb/1e6:.2f} M insns/picture")
print("   A insns    B insns    delta  A calls  function")
for k in sorted(set(a) | set(b), key=lambda k: -(a[k][1] - b[k][1]))[:35]:
    print(f"{a[k][1]:10.0f} {b[k][1]:10.0f} {a[k][1]-b[k][1]:+9.0f} {a[k][0]:8.0f}  {k} {a[k][2] or b[k][2]}")
