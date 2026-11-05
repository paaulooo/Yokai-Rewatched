import re,sys,csv,collections
# uso: python parse_trace.py ipc_trace.txt ipc_commands.csv saida.csv
tr=open(sys.argv[1],errors="replace").read().split("\n")
names=[];handles={};site={}
pend=None
for l in tr:
    if l.startswith("SRVNAME "): pend=re.sub(r"[^\x20-\x7e]","",l[8:]).strip()
    elif l.startswith("SRVHANDLE ") and pend:
        h=int(l.split()[1],16)
        if h: handles[h]=pend
        pend=None
    elif l.startswith("SVC32"):
        m=re.match(r"SVC32 pc=(0x[0-9a-f]+) r0=(0x[0-9a-f]+)",l)
        if m: site[int(m.group(1),16)]=int(m.group(2),16)
# funcao dona de cada svc: maior inicio de funcao <= pc
rows=list(csv.DictReader(open(sys.argv[2])))
fs=sorted((int(r["func"],16),r) for r in rows)
import bisect
starts=[f for f,_ in fs]
res=collections.OrderedDict();orphans=[]
for pc,h in site.items():
    i=bisect.bisect_right(starts,pc)-1
    svcname=handles.get(h,"?handle 0x%x"%h)
    if i<0 or pc-fs[i][0]>0x100:
        orphans.append((pc,svcname)); continue
    f,r=fs[i]
    res.setdefault(f,(svcname,r))
w=csv.writer(open(sys.argv[3],"w",newline=""))
w.writerow(["func","service","header","cmd_id"])
for f,(s,r) in sorted(res.items()): w.writerow(["%08x"%f,s,r["header"],r["cmd_id"]])
open(sys.argv[3].replace(".csv","_orphans.csv"),"w").write("pc,service\n"+"\n".join("%08x,%s"%o for o in sorted(orphans))+"\n")
print(len(orphans),"svc fora de funcoes do CSV (ver *_orphans.csv)")
print(len(res),"funcoes com servico;",len(handles),"handles de servico")
