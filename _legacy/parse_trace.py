import re,sys,csv,collections
# uso: python parse_trace.py ipc_trace.txt ipc_commands.csv saida.csv
tr=open(sys.argv[1],errors="replace").read().split("\n")
names=[];handles={};site={}
pend=None
for l in tr:
    if l.startswith("SRVNAME "): pend=l[8:].strip()
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
res=collections.OrderedDict()
for pc,h in site.items():
    i=bisect.bisect_right(starts,pc)-1
    if i<0: continue
    f,r=fs[i]
    res[f]=(handles.get(h,"?handle 0x%x"%h),r)
w=csv.writer(open(sys.argv[3],"w",newline=""))
w.writerow(["func","service","header","cmd_id"])
for f,(s,r) in sorted(res.items()): w.writerow(["%08x"%f,s,r["header"],r["cmd_id"]])
print(len(res),"funcoes com servico;",len(handles),"handles de servico")
