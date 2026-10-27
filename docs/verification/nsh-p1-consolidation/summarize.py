import re, sys
cur=None; tot=[0,0,0]; rows=[]
pkg_of={}
for line in open(sys.argv[1], errors='replace'):
    m=re.match(r'\s+(Running|Doc-tests) (.*?)( \(target/debug/deps/([^)]*)\))?$', line.rstrip())
    if m:
        cur=(m.group(2), (m.group(4) or '').rsplit('-',1)[0]); continue
    m=re.match(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored', line)
    if m and cur:
        p,f,i=map(int,m.group(2,3,4)); tot[0]+=p; tot[1]+=f; tot[2]+=i
        if p or f or i: rows.append(f'{cur[1] or cur[0]:40s} {cur[0]:45s} {p:5d} passed {f:3d} failed {i:3d} ignored')
for r in rows: print(r)
print(f'TOTAL {tot[0]} passed, {tot[1]} failed, {tot[2]} ignored')
