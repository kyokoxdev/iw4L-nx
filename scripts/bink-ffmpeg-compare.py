#!/usr/bin/env python3
"""Compare crates/bink constant tables against FFmpeg's Bink data files.

Usage: scripts/bink-ffmpeg-compare.py <ffmpeg-libavcodec-dir> <crates/bink/src>
Fetch the inputs first (pinned tag n7.1):
  for f in binkdata.h binkaudio.c wma_freqs.c; do
    curl -fO https://raw.githubusercontent.com/FFmpeg/FFmpeg/n7.1/libavcodec/$f
  done
Prints IDENTICAL / DIFF per table. See docs/provenance/bink.md.
"""
import re,sys
F=sys.argv[1]; R=sys.argv[2]
def nums(txt):
    return [int(x,0) for x in re.findall(r'-?0[xX][0-9a-fA-F]+|-?\d+', re.sub(r'//[^\n]*|/\*.*?\*/','',txt,flags=re.S))]
def c_arr(path,name):
    t=open(path).read()
    m=re.search(name+r'\s*(\[[^=]*\])\s*=\s*\{',t)
    i=m.end(); d=1; j=i
    while d: 
        d+= (t[j]=='{')-(t[j]=='}'); j+=1
    return nums(t[i:j-1])
def r_arr(path,name):
    t=open(path).read()
    m=re.search(r'const '+name+r':[^=]*=\s*',t)
    i=m.end(); d=0; j=i
    while True:
        c=t[j]; d+=(c=='[')-(c==']'); j+=1
        if d==0 and c==']': break
    return nums(t[i:j])
pairs=[('bink_scan','SCAN','binkdata.h','tables.rs'),('bink_tree_bits','TREE_BITS','binkdata.h','tables.rs'),
('bink_tree_lens','TREE_LENS','binkdata.h','tables.rs'),('bink_patterns','PATTERNS','binkdata.h','tables.rs'),
('bink_intra_quant','INTRA_QUANT','binkdata.h','tables.rs'),('bink_inter_quant','INTER_QUANT','binkdata.h','tables.rs')]
for cn,rn,cf,rf in pairs:
    a=c_arr(F+'/'+cf,cn); 
    b=r_arr(R+'/'+rf,rn)
    print(rn,len(a),len(b),'IDENTICAL' if a==b else 'DIFF at '+str([i for i,(x,y) in enumerate(zip(a,b)) if x!=y][:10]))
a=c_arr(F+'/wma_freqs.c','ff_wma_critical_freqs'); b=r_arr(R+'/audio.rs','CRITICAL_FREQS'); print('CRITICAL_FREQS',a==b)
a=c_arr(F+'/binkaudio.c','rle_length_tab'); b=r_arr(R+'/audio.rs','RLE_LENS'); print('RLE_LENS',a==b,a,b)
