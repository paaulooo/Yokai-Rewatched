import re,sys,csv,collections
src=sys.argv[1]; out=sys.argv[2]
t=open(src,errors='replace').read()
# split into functions by top-level signature lines
parts=re.split(r'\n(?=[A-Za-z_][^\n(]*\bFUN_[0-9a-f]{8}\()',t)
rows=[];hist=collections.Counter()
for p in parts:
    m=re.match(r'[^\n]*\bFUN_([0-9a-f]{8})\(',p)
    if not m or 'software_interrupt(0x32)' not in p: continue
    addr=m.group(1)
    h=re.search(r'\+ 0x80\) = (0x[0-9a-f]+|\d+);',p)
    if not h: rows.append((addr,'','','','')); continue
    v=int(h.group(1),0)
    cmd,norm,tr=v>>16,(v>>6)&0x3f,v&0x3f
    rows.append((addr,hex(v),hex(cmd),norm,tr)); hist[(hex(v))]+=1
with open(out,'w',newline='') as f:
    w=csv.writer(f);w.writerow(['func','header','cmd_id','normal_params','translate_params']);w.writerows(rows)
print("funcs with SendSyncRequest:",len(rows),"| with static header:",sum(1 for r in rows if r[1]),"| distinct headers:",len(hist))
for h,c in hist.most_common(12):print(h,c)
