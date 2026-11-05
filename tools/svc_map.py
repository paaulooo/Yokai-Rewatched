import re,sys,collections,csv
t=open(sys.argv[1],errors='replace').read()
lines=t.split('\n')
# functions
def fname_at(i):
    for j in range(i,0,-1):
        m=re.match(r'[A-Za-z_][^\n(]*\bFUN_([0-9a-f]{8})\(',lines[j])
        if m: return m.group(1)
# wrapper functions that bind handle: those calling 00333bc4 with param_1,param_2
wr=set()
for i,l in enumerate(lines):
    if re.search(r'FUN_00333bc4\(param_1,',l): wr.add(fname_at(i))
print("wrappers",wr,file=sys.stderr)
wr|={'00333bc4','0011c828'}
# string table: DAT address -> literal
strs={}
for m in re.finditer(r's_([^\n;]*?)_([0-9a-f]{8});',t): strs[m.group(2)]=m.group(1)
handle2svc={}
pat=re.compile(r'FUN_(%s)\(\(?(?:undefined4 \*\)|int \*\))?&?(DAT_[0-9a-f]{8}|param_\d|\w+)\)?,(?:\(undefined4 \*\)|\(char \*\))?(?:"([^"]+)"|&(DAT_[0-9a-f]{8}))'%'|'.join(wr))
for m in re.finditer(r'FUN_(?:%s)\(\(?[a-z0-9 ]*\*?\)?&?(DAT_[0-9a-f]{8})\s*,\s*(?:\([a-z0-9 ]*\*\))?(?:"([^"]+)"|&(DAT_[0-9a-f]{8}))'%'|'.join(wr),t):
    h,s,d=m.groups()
    name=s or ('?'+d)
    handle2svc.setdefault(h,set()).add(name)
for k,v in sorted(handle2svc.items()): print(k,v,file=sys.stderr)
import json
json.dump({k:sorted(v) for k,v in handle2svc.items()},open(sys.argv[2],'w'))
