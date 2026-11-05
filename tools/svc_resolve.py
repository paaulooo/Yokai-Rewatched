import re,sys,json,csv,collections
src,h2s_f,ipc_f,out=sys.argv[1:5]
t=open(src,errors='replace').read()
h2s=json.load(open(h2s_f)); h2s['DAT_005d78e0']=['ac:u']
parts=re.split(r'\n(?=[A-Za-z_][^\n(]*\bFUN_[0-9a-f]{8}\()',t)
body={}
for p in parts:
    m=re.match(r'[^\n]*\bFUN_([0-9a-f]{8})\(',p)
    if m: body[m.group(1)]=p
rows=list(csv.DictReader(open(ipc_f)))
def direct(b):
    # handle read right before svc 0x32
    pre=b.split('software_interrupt(0x32)')[0]
    g=re.findall(r'(DAT_[0-9a-f]{8})',pre[-400:])
    return [x for x in g if x in h2s]
callers=collections.defaultdict(list)
for a,b in body.items():
    for m in re.finditer(r'FUN_([0-9a-f]{8})\(\(?[a-z0-9 ]*\*?\)?&(DAT_[0-9a-f]{8})',b):
        callers[m.group(1)].append(m.group(2))
res=[];cnt=collections.Counter()
for r in rows:
    f=r['func'];b=body.get(f,'');svc=''
    d=direct(b)
    if d: svc=h2s[d[-1]][0]; how='global'
    else:
        c=[x for x in callers.get(f,[]) if x in h2s]
        if c:
            s=collections.Counter(h2s[x][0] for x in c); svc=s.most_common(1)[0][0]; how='caller'
        else: how='unresolved'
    cnt[svc or '?']+=1
    r['service']=svc;r['how']=how;res.append(r)
w=csv.DictWriter(open(out,'w',newline=''),fieldnames=list(rows[0].keys())+['service','how']);w.writeheader();w.writerows(res)
for k,v in cnt.most_common():print(k,v)
