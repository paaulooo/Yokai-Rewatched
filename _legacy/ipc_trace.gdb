# Gerado a partir do code.bin. Uso: gdb-multiarch -x ipc_trace.gdb
set pagination off
set confirm off
set architecture arm
set endian little
set logging file ipc_trace.txt
set logging overwrite on
set logging redirect on
set logging enabled on
target remote :24689

break *0x3107a8
commands
silent
printf "SRVNAME %s\n", (char*)($r4+4)
continue
end
break *0x3107b8
commands
silent
printf "SRVHANDLE 0x%x\n", $r0
continue
end

tbreak *0x106374
commands
silent
printf "SVC32 pc=0x106374 r0=0x%x\n", $r0
continue
end
tbreak *0x106b9c
commands
silent
printf "SVC32 pc=0x106b9c r0=0x%x\n", $r0
continue
end
tbreak *0x106fc8
commands
silent
printf "SVC32 pc=0x106fc8 r0=0x%x\n", $r0
continue
end
tbreak *0x10701c
commands
silent
printf "SVC32 pc=0x10701c r0=0x%x\n", $r0
continue
end
tbreak *0x107088
commands
silent
printf "SVC32 pc=0x107088 r0=0x%x\n", $r0
continue
end
tbreak *0x1070c4
commands
silent
printf "SVC32 pc=0x1070c4 r0=0x%x\n", $r0
continue
end
tbreak *0x1070fc
commands
silent
printf "SVC32 pc=0x1070fc r0=0x%x\n", $r0
continue
end
tbreak *0x10d0c8
commands
silent
printf "SVC32 pc=0x10d0c8 r0=0x%x\n", $r0
continue
end
tbreak *0x10d4a0
commands
silent
printf "SVC32 pc=0x10d4a0 r0=0x%x\n", $r0
continue
end
tbreak *0x10d4e4
commands
silent
printf "SVC32 pc=0x10d4e4 r0=0x%x\n", $r0
continue
end
tbreak *0x10d528
commands
silent
printf "SVC32 pc=0x10d528 r0=0x%x\n", $r0
continue
end
tbreak *0x10d56c
commands
silent
printf "SVC32 pc=0x10d56c r0=0x%x\n", $r0
continue
end
tbreak *0x10d894
commands
silent
printf "SVC32 pc=0x10d894 r0=0x%x\n", $r0
continue
end
tbreak *0x10d8fc
commands
silent
printf "SVC32 pc=0x10d8fc r0=0x%x\n", $r0
continue
end
tbreak *0x10d938
commands
silent
printf "SVC32 pc=0x10d938 r0=0x%x\n", $r0
continue
end
tbreak *0x10d974
commands
silent
printf "SVC32 pc=0x10d974 r0=0x%x\n", $r0
continue
end
tbreak *0x10d9ac
commands
silent
printf "SVC32 pc=0x10d9ac r0=0x%x\n", $r0
continue
end
tbreak *0x10d9e4
commands
silent
printf "SVC32 pc=0x10d9e4 r0=0x%x\n", $r0
continue
end
tbreak *0x10e6e4
commands
silent
printf "SVC32 pc=0x10e6e4 r0=0x%x\n", $r0
continue
end
tbreak *0x10e71c
commands
silent
printf "SVC32 pc=0x10e71c r0=0x%x\n", $r0
continue
end
tbreak *0x116574
commands
silent
printf "SVC32 pc=0x116574 r0=0x%x\n", $r0
continue
end
tbreak *0x1165a4
commands
silent
printf "SVC32 pc=0x1165a4 r0=0x%x\n", $r0
continue
end
tbreak *0x1165d4
commands
silent
printf "SVC32 pc=0x1165d4 r0=0x%x\n", $r0
continue
end
tbreak *0x116604
commands
silent
printf "SVC32 pc=0x116604 r0=0x%x\n", $r0
continue
end
tbreak *0x116c00
commands
silent
printf "SVC32 pc=0x116c00 r0=0x%x\n", $r0
continue
end
tbreak *0x1178bc
commands
silent
printf "SVC32 pc=0x1178bc r0=0x%x\n", $r0
continue
end
tbreak *0x1178f4
commands
silent
printf "SVC32 pc=0x1178f4 r0=0x%x\n", $r0
continue
end
tbreak *0x117944
commands
silent
printf "SVC32 pc=0x117944 r0=0x%x\n", $r0
continue
end
tbreak *0x117980
commands
silent
printf "SVC32 pc=0x117980 r0=0x%x\n", $r0
continue
end
tbreak *0x1179c0
commands
silent
printf "SVC32 pc=0x1179c0 r0=0x%x\n", $r0
continue
end
tbreak *0x11c6bc
commands
silent
printf "SVC32 pc=0x11c6bc r0=0x%x\n", $r0
continue
end
tbreak *0x11ca0c
commands
silent
printf "SVC32 pc=0x11ca0c r0=0x%x\n", $r0
continue
end
tbreak *0x11d4c4
commands
silent
printf "SVC32 pc=0x11d4c4 r0=0x%x\n", $r0
continue
end
tbreak *0x11d4f4
commands
silent
printf "SVC32 pc=0x11d4f4 r0=0x%x\n", $r0
continue
end
tbreak *0x11d524
commands
silent
printf "SVC32 pc=0x11d524 r0=0x%x\n", $r0
continue
end
tbreak *0x11d568
commands
silent
printf "SVC32 pc=0x11d568 r0=0x%x\n", $r0
continue
end
tbreak *0x11d604
commands
silent
printf "SVC32 pc=0x11d604 r0=0x%x\n", $r0
continue
end
tbreak *0x11d634
commands
silent
printf "SVC32 pc=0x11d634 r0=0x%x\n", $r0
continue
end
tbreak *0x11d664
commands
silent
printf "SVC32 pc=0x11d664 r0=0x%x\n", $r0
continue
end
tbreak *0x11d6a8
commands
silent
printf "SVC32 pc=0x11d6a8 r0=0x%x\n", $r0
continue
end
tbreak *0x11d808
commands
silent
printf "SVC32 pc=0x11d808 r0=0x%x\n", $r0
continue
end
tbreak *0x11d830
commands
silent
printf "SVC32 pc=0x11d830 r0=0x%x\n", $r0
continue
end
tbreak *0x11d974
commands
silent
printf "SVC32 pc=0x11d974 r0=0x%x\n", $r0
continue
end
tbreak *0x11d9d8
commands
silent
printf "SVC32 pc=0x11d9d8 r0=0x%x\n", $r0
continue
end
tbreak *0x11da2c
commands
silent
printf "SVC32 pc=0x11da2c r0=0x%x\n", $r0
continue
end
tbreak *0x11da7c
commands
silent
printf "SVC32 pc=0x11da7c r0=0x%x\n", $r0
continue
end
tbreak *0x11daf0
commands
silent
printf "SVC32 pc=0x11daf0 r0=0x%x\n", $r0
continue
end
tbreak *0x11db54
commands
silent
printf "SVC32 pc=0x11db54 r0=0x%x\n", $r0
continue
end
tbreak *0x11dba8
commands
silent
printf "SVC32 pc=0x11dba8 r0=0x%x\n", $r0
continue
end
tbreak *0x11dbdc
commands
silent
printf "SVC32 pc=0x11dbdc r0=0x%x\n", $r0
continue
end
tbreak *0x11dc10
commands
silent
printf "SVC32 pc=0x11dc10 r0=0x%x\n", $r0
continue
end
tbreak *0x11dc48
commands
silent
printf "SVC32 pc=0x11dc48 r0=0x%x\n", $r0
continue
end
tbreak *0x11dc80
commands
silent
printf "SVC32 pc=0x11dc80 r0=0x%x\n", $r0
continue
end
tbreak *0x11dcc0
commands
silent
printf "SVC32 pc=0x11dcc0 r0=0x%x\n", $r0
continue
end
tbreak *0x11dcf8
commands
silent
printf "SVC32 pc=0x11dcf8 r0=0x%x\n", $r0
continue
end
tbreak *0x120eac
commands
silent
printf "SVC32 pc=0x120eac r0=0x%x\n", $r0
continue
end
tbreak *0x121238
commands
silent
printf "SVC32 pc=0x121238 r0=0x%x\n", $r0
continue
end
tbreak *0x121278
commands
silent
printf "SVC32 pc=0x121278 r0=0x%x\n", $r0
continue
end
tbreak *0x121e50
commands
silent
printf "SVC32 pc=0x121e50 r0=0x%x\n", $r0
continue
end
tbreak *0x121e80
commands
silent
printf "SVC32 pc=0x121e80 r0=0x%x\n", $r0
continue
end
tbreak *0x121eb4
commands
silent
printf "SVC32 pc=0x121eb4 r0=0x%x\n", $r0
continue
end
tbreak *0x121ef4
commands
silent
printf "SVC32 pc=0x121ef4 r0=0x%x\n", $r0
continue
end
tbreak *0x122038
commands
silent
printf "SVC32 pc=0x122038 r0=0x%x\n", $r0
continue
end
tbreak *0x122068
commands
silent
printf "SVC32 pc=0x122068 r0=0x%x\n", $r0
continue
end
tbreak *0x12bcd8
commands
silent
printf "SVC32 pc=0x12bcd8 r0=0x%x\n", $r0
continue
end
tbreak *0x12bd10
commands
silent
printf "SVC32 pc=0x12bd10 r0=0x%x\n", $r0
continue
end
tbreak *0x12c664
commands
silent
printf "SVC32 pc=0x12c664 r0=0x%x\n", $r0
continue
end
tbreak *0x12c6bc
commands
silent
printf "SVC32 pc=0x12c6bc r0=0x%x\n", $r0
continue
end
tbreak *0x12c700
commands
silent
printf "SVC32 pc=0x12c700 r0=0x%x\n", $r0
continue
end
tbreak *0x12c9e0
commands
silent
printf "SVC32 pc=0x12c9e0 r0=0x%x\n", $r0
continue
end
tbreak *0x133998
commands
silent
printf "SVC32 pc=0x133998 r0=0x%x\n", $r0
continue
end
tbreak *0x1339d8
commands
silent
printf "SVC32 pc=0x1339d8 r0=0x%x\n", $r0
continue
end
tbreak *0x133a24
commands
silent
printf "SVC32 pc=0x133a24 r0=0x%x\n", $r0
continue
end
tbreak *0x133a54
commands
silent
printf "SVC32 pc=0x133a54 r0=0x%x\n", $r0
continue
end
tbreak *0x133a94
commands
silent
printf "SVC32 pc=0x133a94 r0=0x%x\n", $r0
continue
end
tbreak *0x133acc
commands
silent
printf "SVC32 pc=0x133acc r0=0x%x\n", $r0
continue
end
tbreak *0x134a9c
commands
silent
printf "SVC32 pc=0x134a9c r0=0x%x\n", $r0
continue
end
tbreak *0x134ae8
commands
silent
printf "SVC32 pc=0x134ae8 r0=0x%x\n", $r0
continue
end
tbreak *0x134b3c
commands
silent
printf "SVC32 pc=0x134b3c r0=0x%x\n", $r0
continue
end
tbreak *0x134b6c
commands
silent
printf "SVC32 pc=0x134b6c r0=0x%x\n", $r0
continue
end
tbreak *0x134be4
commands
silent
printf "SVC32 pc=0x134be4 r0=0x%x\n", $r0
continue
end
tbreak *0x1380c4
commands
silent
printf "SVC32 pc=0x1380c4 r0=0x%x\n", $r0
continue
end
tbreak *0x138124
commands
silent
printf "SVC32 pc=0x138124 r0=0x%x\n", $r0
continue
end
tbreak *0x138164
commands
silent
printf "SVC32 pc=0x138164 r0=0x%x\n", $r0
continue
end
tbreak *0x13894c
commands
silent
printf "SVC32 pc=0x13894c r0=0x%x\n", $r0
continue
end
tbreak *0x13897c
commands
silent
printf "SVC32 pc=0x13897c r0=0x%x\n", $r0
continue
end
tbreak *0x139318
commands
silent
printf "SVC32 pc=0x139318 r0=0x%x\n", $r0
continue
end
tbreak *0x139378
commands
silent
printf "SVC32 pc=0x139378 r0=0x%x\n", $r0
continue
end
tbreak *0x13ba3c
commands
silent
printf "SVC32 pc=0x13ba3c r0=0x%x\n", $r0
continue
end
tbreak *0x13ba7c
commands
silent
printf "SVC32 pc=0x13ba7c r0=0x%x\n", $r0
continue
end
tbreak *0x13babc
commands
silent
printf "SVC32 pc=0x13babc r0=0x%x\n", $r0
continue
end
tbreak *0x13d620
commands
silent
printf "SVC32 pc=0x13d620 r0=0x%x\n", $r0
continue
end
tbreak *0x13d648
commands
silent
printf "SVC32 pc=0x13d648 r0=0x%x\n", $r0
continue
end
tbreak *0x141184
commands
silent
printf "SVC32 pc=0x141184 r0=0x%x\n", $r0
continue
end
tbreak *0x22d4c8
commands
silent
printf "SVC32 pc=0x22d4c8 r0=0x%x\n", $r0
continue
end
tbreak *0x22d50c
commands
silent
printf "SVC32 pc=0x22d50c r0=0x%x\n", $r0
continue
end
tbreak *0x22d568
commands
silent
printf "SVC32 pc=0x22d568 r0=0x%x\n", $r0
continue
end
tbreak *0x22d5c4
commands
silent
printf "SVC32 pc=0x22d5c4 r0=0x%x\n", $r0
continue
end
tbreak *0x22d608
commands
silent
printf "SVC32 pc=0x22d608 r0=0x%x\n", $r0
continue
end
tbreak *0x22d644
commands
silent
printf "SVC32 pc=0x22d644 r0=0x%x\n", $r0
continue
end
tbreak *0x22d688
commands
silent
printf "SVC32 pc=0x22d688 r0=0x%x\n", $r0
continue
end
tbreak *0x22d6cc
commands
silent
printf "SVC32 pc=0x22d6cc r0=0x%x\n", $r0
continue
end
tbreak *0x22d710
commands
silent
printf "SVC32 pc=0x22d710 r0=0x%x\n", $r0
continue
end
tbreak *0x22d74c
commands
silent
printf "SVC32 pc=0x22d74c r0=0x%x\n", $r0
continue
end
tbreak *0x22d798
commands
silent
printf "SVC32 pc=0x22d798 r0=0x%x\n", $r0
continue
end
tbreak *0x22d800
commands
silent
printf "SVC32 pc=0x22d800 r0=0x%x\n", $r0
continue
end
tbreak *0x22f210
commands
silent
printf "SVC32 pc=0x22f210 r0=0x%x\n", $r0
continue
end
tbreak *0x22f260
commands
silent
printf "SVC32 pc=0x22f260 r0=0x%x\n", $r0
continue
end
tbreak *0x22f2cc
commands
silent
printf "SVC32 pc=0x22f2cc r0=0x%x\n", $r0
continue
end
tbreak *0x22f348
commands
silent
printf "SVC32 pc=0x22f348 r0=0x%x\n", $r0
continue
end
tbreak *0x22f3bc
commands
silent
printf "SVC32 pc=0x22f3bc r0=0x%x\n", $r0
continue
end
tbreak *0x22f414
commands
silent
printf "SVC32 pc=0x22f414 r0=0x%x\n", $r0
continue
end
tbreak *0x22f444
commands
silent
printf "SVC32 pc=0x22f444 r0=0x%x\n", $r0
continue
end
tbreak *0x22f47c
commands
silent
printf "SVC32 pc=0x22f47c r0=0x%x\n", $r0
continue
end
tbreak *0x22f4d8
commands
silent
printf "SVC32 pc=0x22f4d8 r0=0x%x\n", $r0
continue
end
tbreak *0x22f528
commands
silent
printf "SVC32 pc=0x22f528 r0=0x%x\n", $r0
continue
end
tbreak *0x22f584
commands
silent
printf "SVC32 pc=0x22f584 r0=0x%x\n", $r0
continue
end
tbreak *0x22f5c0
commands
silent
printf "SVC32 pc=0x22f5c0 r0=0x%x\n", $r0
continue
end
tbreak *0x22f5f8
commands
silent
printf "SVC32 pc=0x22f5f8 r0=0x%x\n", $r0
continue
end
tbreak *0x22f660
commands
silent
printf "SVC32 pc=0x22f660 r0=0x%x\n", $r0
continue
end
tbreak *0x22f6e8
commands
silent
printf "SVC32 pc=0x22f6e8 r0=0x%x\n", $r0
continue
end
tbreak *0x22f720
commands
silent
printf "SVC32 pc=0x22f720 r0=0x%x\n", $r0
continue
end
tbreak *0x22f750
commands
silent
printf "SVC32 pc=0x22f750 r0=0x%x\n", $r0
continue
end
tbreak *0x22f7a8
commands
silent
printf "SVC32 pc=0x22f7a8 r0=0x%x\n", $r0
continue
end
tbreak *0x22f7dc
commands
silent
printf "SVC32 pc=0x22f7dc r0=0x%x\n", $r0
continue
end
tbreak *0x22f834
commands
silent
printf "SVC32 pc=0x22f834 r0=0x%x\n", $r0
continue
end
tbreak *0x22f86c
commands
silent
printf "SVC32 pc=0x22f86c r0=0x%x\n", $r0
continue
end
tbreak *0x22f8c0
commands
silent
printf "SVC32 pc=0x22f8c0 r0=0x%x\n", $r0
continue
end
tbreak *0x22f8f4
commands
silent
printf "SVC32 pc=0x22f8f4 r0=0x%x\n", $r0
continue
end
tbreak *0x232bec
commands
silent
printf "SVC32 pc=0x232bec r0=0x%x\n", $r0
continue
end
tbreak *0x232c30
commands
silent
printf "SVC32 pc=0x232c30 r0=0x%x\n", $r0
continue
end
tbreak *0x232c9c
commands
silent
printf "SVC32 pc=0x232c9c r0=0x%x\n", $r0
continue
end
tbreak *0x232cfc
commands
silent
printf "SVC32 pc=0x232cfc r0=0x%x\n", $r0
continue
end
tbreak *0x232d74
commands
silent
printf "SVC32 pc=0x232d74 r0=0x%x\n", $r0
continue
end
tbreak *0x232db4
commands
silent
printf "SVC32 pc=0x232db4 r0=0x%x\n", $r0
continue
end
tbreak *0x232e00
commands
silent
printf "SVC32 pc=0x232e00 r0=0x%x\n", $r0
continue
end
tbreak *0x232e40
commands
silent
printf "SVC32 pc=0x232e40 r0=0x%x\n", $r0
continue
end
tbreak *0x232e78
commands
silent
printf "SVC32 pc=0x232e78 r0=0x%x\n", $r0
continue
end
tbreak *0x232ed0
commands
silent
printf "SVC32 pc=0x232ed0 r0=0x%x\n", $r0
continue
end
tbreak *0x232f1c
commands
silent
printf "SVC32 pc=0x232f1c r0=0x%x\n", $r0
continue
end
tbreak *0x232f78
commands
silent
printf "SVC32 pc=0x232f78 r0=0x%x\n", $r0
continue
end
tbreak *0x233064
commands
silent
printf "SVC32 pc=0x233064 r0=0x%x\n", $r0
continue
end
tbreak *0x2330a8
commands
silent
printf "SVC32 pc=0x2330a8 r0=0x%x\n", $r0
continue
end
tbreak *0x233114
commands
silent
printf "SVC32 pc=0x233114 r0=0x%x\n", $r0
continue
end
tbreak *0x233174
commands
silent
printf "SVC32 pc=0x233174 r0=0x%x\n", $r0
continue
end
tbreak *0x2331ec
commands
silent
printf "SVC32 pc=0x2331ec r0=0x%x\n", $r0
continue
end
tbreak *0x23322c
commands
silent
printf "SVC32 pc=0x23322c r0=0x%x\n", $r0
continue
end
tbreak *0x233278
commands
silent
printf "SVC32 pc=0x233278 r0=0x%x\n", $r0
continue
end
tbreak *0x2332b8
commands
silent
printf "SVC32 pc=0x2332b8 r0=0x%x\n", $r0
continue
end
tbreak *0x2332f0
commands
silent
printf "SVC32 pc=0x2332f0 r0=0x%x\n", $r0
continue
end
tbreak *0x233348
commands
silent
printf "SVC32 pc=0x233348 r0=0x%x\n", $r0
continue
end
tbreak *0x233394
commands
silent
printf "SVC32 pc=0x233394 r0=0x%x\n", $r0
continue
end
tbreak *0x2333f0
commands
silent
printf "SVC32 pc=0x2333f0 r0=0x%x\n", $r0
continue
end
tbreak *0x233f84
commands
silent
printf "SVC32 pc=0x233f84 r0=0x%x\n", $r0
continue
end
tbreak *0x233fc8
commands
silent
printf "SVC32 pc=0x233fc8 r0=0x%x\n", $r0
continue
end
tbreak *0x234008
commands
silent
printf "SVC32 pc=0x234008 r0=0x%x\n", $r0
continue
end
tbreak *0x23a154
commands
silent
printf "SVC32 pc=0x23a154 r0=0x%x\n", $r0
continue
end
tbreak *0x23a1b8
commands
silent
printf "SVC32 pc=0x23a1b8 r0=0x%x\n", $r0
continue
end
tbreak *0x23a1e8
commands
silent
printf "SVC32 pc=0x23a1e8 r0=0x%x\n", $r0
continue
end
tbreak *0x23a240
commands
silent
printf "SVC32 pc=0x23a240 r0=0x%x\n", $r0
continue
end
tbreak *0x23a274
commands
silent
printf "SVC32 pc=0x23a274 r0=0x%x\n", $r0
continue
end
tbreak *0x23a2b4
commands
silent
printf "SVC32 pc=0x23a2b4 r0=0x%x\n", $r0
continue
end
tbreak *0x23a2e4
commands
silent
printf "SVC32 pc=0x23a2e4 r0=0x%x\n", $r0
continue
end
tbreak *0x23a368
commands
silent
printf "SVC32 pc=0x23a368 r0=0x%x\n", $r0
continue
end
tbreak *0x23a3bc
commands
silent
printf "SVC32 pc=0x23a3bc r0=0x%x\n", $r0
continue
end
tbreak *0x23a3fc
commands
silent
printf "SVC32 pc=0x23a3fc r0=0x%x\n", $r0
continue
end
tbreak *0x23a444
commands
silent
printf "SVC32 pc=0x23a444 r0=0x%x\n", $r0
continue
end
tbreak *0x23a4bc
commands
silent
printf "SVC32 pc=0x23a4bc r0=0x%x\n", $r0
continue
end
tbreak *0x23a4f4
commands
silent
printf "SVC32 pc=0x23a4f4 r0=0x%x\n", $r0
continue
end
tbreak *0x23a534
commands
silent
printf "SVC32 pc=0x23a534 r0=0x%x\n", $r0
continue
end
tbreak *0x23a5c0
commands
silent
printf "SVC32 pc=0x23a5c0 r0=0x%x\n", $r0
continue
end
tbreak *0x23a88c
commands
silent
printf "SVC32 pc=0x23a88c r0=0x%x\n", $r0
continue
end
tbreak *0x23a8e4
commands
silent
printf "SVC32 pc=0x23a8e4 r0=0x%x\n", $r0
continue
end
tbreak *0x23a940
commands
silent
printf "SVC32 pc=0x23a940 r0=0x%x\n", $r0
continue
end
tbreak *0x23a99c
commands
silent
printf "SVC32 pc=0x23a99c r0=0x%x\n", $r0
continue
end
tbreak *0x23a9f8
commands
silent
printf "SVC32 pc=0x23a9f8 r0=0x%x\n", $r0
continue
end
tbreak *0x23aa38
commands
silent
printf "SVC32 pc=0x23aa38 r0=0x%x\n", $r0
continue
end
tbreak *0x23aa90
commands
silent
printf "SVC32 pc=0x23aa90 r0=0x%x\n", $r0
continue
end
tbreak *0x23aad0
commands
silent
printf "SVC32 pc=0x23aad0 r0=0x%x\n", $r0
continue
end
tbreak *0x23ab04
commands
silent
printf "SVC32 pc=0x23ab04 r0=0x%x\n", $r0
continue
end
tbreak *0x23ab3c
commands
silent
printf "SVC32 pc=0x23ab3c r0=0x%x\n", $r0
continue
end
tbreak *0x23ab70
commands
silent
printf "SVC32 pc=0x23ab70 r0=0x%x\n", $r0
continue
end
tbreak *0x23aba4
commands
silent
printf "SVC32 pc=0x23aba4 r0=0x%x\n", $r0
continue
end
tbreak *0x23abe4
commands
silent
printf "SVC32 pc=0x23abe4 r0=0x%x\n", $r0
continue
end
tbreak *0x23ac20
commands
silent
printf "SVC32 pc=0x23ac20 r0=0x%x\n", $r0
continue
end
tbreak *0x23ac5c
commands
silent
printf "SVC32 pc=0x23ac5c r0=0x%x\n", $r0
continue
end
tbreak *0x23ac98
commands
silent
printf "SVC32 pc=0x23ac98 r0=0x%x\n", $r0
continue
end
tbreak *0x23c334
commands
silent
printf "SVC32 pc=0x23c334 r0=0x%x\n", $r0
continue
end
tbreak *0x23c374
commands
silent
printf "SVC32 pc=0x23c374 r0=0x%x\n", $r0
continue
end
tbreak *0x23c3ac
commands
silent
printf "SVC32 pc=0x23c3ac r0=0x%x\n", $r0
continue
end
tbreak *0x23c3fc
commands
silent
printf "SVC32 pc=0x23c3fc r0=0x%x\n", $r0
continue
end
tbreak *0x23c450
commands
silent
printf "SVC32 pc=0x23c450 r0=0x%x\n", $r0
continue
end
tbreak *0x23c4a8
commands
silent
printf "SVC32 pc=0x23c4a8 r0=0x%x\n", $r0
continue
end
tbreak *0x23c510
commands
silent
printf "SVC32 pc=0x23c510 r0=0x%x\n", $r0
continue
end
tbreak *0x23c55c
commands
silent
printf "SVC32 pc=0x23c55c r0=0x%x\n", $r0
continue
end
tbreak *0x23c5a8
commands
silent
printf "SVC32 pc=0x23c5a8 r0=0x%x\n", $r0
continue
end
tbreak *0x23c5f4
commands
silent
printf "SVC32 pc=0x23c5f4 r0=0x%x\n", $r0
continue
end
tbreak *0x23c628
commands
silent
printf "SVC32 pc=0x23c628 r0=0x%x\n", $r0
continue
end
tbreak *0x23c688
commands
silent
printf "SVC32 pc=0x23c688 r0=0x%x\n", $r0
continue
end
tbreak *0x23c6d8
commands
silent
printf "SVC32 pc=0x23c6d8 r0=0x%x\n", $r0
continue
end
tbreak *0x23c734
commands
silent
printf "SVC32 pc=0x23c734 r0=0x%x\n", $r0
continue
end
tbreak *0x23c784
commands
silent
printf "SVC32 pc=0x23c784 r0=0x%x\n", $r0
continue
end
tbreak *0x23c7c8
commands
silent
printf "SVC32 pc=0x23c7c8 r0=0x%x\n", $r0
continue
end
tbreak *0x23c818
commands
silent
printf "SVC32 pc=0x23c818 r0=0x%x\n", $r0
continue
end
tbreak *0x23c864
commands
silent
printf "SVC32 pc=0x23c864 r0=0x%x\n", $r0
continue
end
tbreak *0x23c8c0
commands
silent
printf "SVC32 pc=0x23c8c0 r0=0x%x\n", $r0
continue
end
tbreak *0x23c92c
commands
silent
printf "SVC32 pc=0x23c92c r0=0x%x\n", $r0
continue
end
tbreak *0x23c98c
commands
silent
printf "SVC32 pc=0x23c98c r0=0x%x\n", $r0
continue
end
tbreak *0x23c9c8
commands
silent
printf "SVC32 pc=0x23c9c8 r0=0x%x\n", $r0
continue
end
tbreak *0x23ca24
commands
silent
printf "SVC32 pc=0x23ca24 r0=0x%x\n", $r0
continue
end
tbreak *0x23ca90
commands
silent
printf "SVC32 pc=0x23ca90 r0=0x%x\n", $r0
continue
end
tbreak *0x23cafc
commands
silent
printf "SVC32 pc=0x23cafc r0=0x%x\n", $r0
continue
end
tbreak *0x23cb3c
commands
silent
printf "SVC32 pc=0x23cb3c r0=0x%x\n", $r0
continue
end
tbreak *0x23d168
commands
silent
printf "SVC32 pc=0x23d168 r0=0x%x\n", $r0
continue
end
tbreak *0x23d1a8
commands
silent
printf "SVC32 pc=0x23d1a8 r0=0x%x\n", $r0
continue
end
tbreak *0x23d1e0
commands
silent
printf "SVC32 pc=0x23d1e0 r0=0x%x\n", $r0
continue
end
tbreak *0x23d230
commands
silent
printf "SVC32 pc=0x23d230 r0=0x%x\n", $r0
continue
end
tbreak *0x23d284
commands
silent
printf "SVC32 pc=0x23d284 r0=0x%x\n", $r0
continue
end
tbreak *0x23d2dc
commands
silent
printf "SVC32 pc=0x23d2dc r0=0x%x\n", $r0
continue
end
tbreak *0x23d344
commands
silent
printf "SVC32 pc=0x23d344 r0=0x%x\n", $r0
continue
end
tbreak *0x23d390
commands
silent
printf "SVC32 pc=0x23d390 r0=0x%x\n", $r0
continue
end
tbreak *0x23d3dc
commands
silent
printf "SVC32 pc=0x23d3dc r0=0x%x\n", $r0
continue
end
tbreak *0x23d428
commands
silent
printf "SVC32 pc=0x23d428 r0=0x%x\n", $r0
continue
end
tbreak *0x23d45c
commands
silent
printf "SVC32 pc=0x23d45c r0=0x%x\n", $r0
continue
end
tbreak *0x23d4bc
commands
silent
printf "SVC32 pc=0x23d4bc r0=0x%x\n", $r0
continue
end
tbreak *0x23d50c
commands
silent
printf "SVC32 pc=0x23d50c r0=0x%x\n", $r0
continue
end
tbreak *0x23d568
commands
silent
printf "SVC32 pc=0x23d568 r0=0x%x\n", $r0
continue
end
tbreak *0x23d5b8
commands
silent
printf "SVC32 pc=0x23d5b8 r0=0x%x\n", $r0
continue
end
tbreak *0x23d5fc
commands
silent
printf "SVC32 pc=0x23d5fc r0=0x%x\n", $r0
continue
end
tbreak *0x23d64c
commands
silent
printf "SVC32 pc=0x23d64c r0=0x%x\n", $r0
continue
end
tbreak *0x23d698
commands
silent
printf "SVC32 pc=0x23d698 r0=0x%x\n", $r0
continue
end
tbreak *0x23d6f4
commands
silent
printf "SVC32 pc=0x23d6f4 r0=0x%x\n", $r0
continue
end
tbreak *0x23d74c
commands
silent
printf "SVC32 pc=0x23d74c r0=0x%x\n", $r0
continue
end
tbreak *0x23d7ac
commands
silent
printf "SVC32 pc=0x23d7ac r0=0x%x\n", $r0
continue
end
tbreak *0x23d818
commands
silent
printf "SVC32 pc=0x23d818 r0=0x%x\n", $r0
continue
end
tbreak *0x23d868
commands
silent
printf "SVC32 pc=0x23d868 r0=0x%x\n", $r0
continue
end
tbreak *0x24995c
commands
silent
printf "SVC32 pc=0x24995c r0=0x%x\n", $r0
continue
end
tbreak *0x2499a4
commands
silent
printf "SVC32 pc=0x2499a4 r0=0x%x\n", $r0
continue
end
tbreak *0x2499f4
commands
silent
printf "SVC32 pc=0x2499f4 r0=0x%x\n", $r0
continue
end
tbreak *0x249a3c
commands
silent
printf "SVC32 pc=0x249a3c r0=0x%x\n", $r0
continue
end
tbreak *0x249a7c
commands
silent
printf "SVC32 pc=0x249a7c r0=0x%x\n", $r0
continue
end
tbreak *0x249ac0
commands
silent
printf "SVC32 pc=0x249ac0 r0=0x%x\n", $r0
continue
end
tbreak *0x249b08
commands
silent
printf "SVC32 pc=0x249b08 r0=0x%x\n", $r0
continue
end
tbreak *0x249b68
commands
silent
printf "SVC32 pc=0x249b68 r0=0x%x\n", $r0
continue
end
tbreak *0x249bb8
commands
silent
printf "SVC32 pc=0x249bb8 r0=0x%x\n", $r0
continue
end
tbreak *0x249bf8
commands
silent
printf "SVC32 pc=0x249bf8 r0=0x%x\n", $r0
continue
end
tbreak *0x249c3c
commands
silent
printf "SVC32 pc=0x249c3c r0=0x%x\n", $r0
continue
end
tbreak *0x249c74
commands
silent
printf "SVC32 pc=0x249c74 r0=0x%x\n", $r0
continue
end
tbreak *0x249cb4
commands
silent
printf "SVC32 pc=0x249cb4 r0=0x%x\n", $r0
continue
end
tbreak *0x249cfc
commands
silent
printf "SVC32 pc=0x249cfc r0=0x%x\n", $r0
continue
end
tbreak *0x249d34
commands
silent
printf "SVC32 pc=0x249d34 r0=0x%x\n", $r0
continue
end
tbreak *0x249d6c
commands
silent
printf "SVC32 pc=0x249d6c r0=0x%x\n", $r0
continue
end
tbreak *0x249dbc
commands
silent
printf "SVC32 pc=0x249dbc r0=0x%x\n", $r0
continue
end
tbreak *0x249e14
commands
silent
printf "SVC32 pc=0x249e14 r0=0x%x\n", $r0
continue
end
tbreak *0x249e5c
commands
silent
printf "SVC32 pc=0x249e5c r0=0x%x\n", $r0
continue
end
tbreak *0x249ebc
commands
silent
printf "SVC32 pc=0x249ebc r0=0x%x\n", $r0
continue
end
tbreak *0x249f04
commands
silent
printf "SVC32 pc=0x249f04 r0=0x%x\n", $r0
continue
end
tbreak *0x249f64
commands
silent
printf "SVC32 pc=0x249f64 r0=0x%x\n", $r0
continue
end
tbreak *0x249fa8
commands
silent
printf "SVC32 pc=0x249fa8 r0=0x%x\n", $r0
continue
end
tbreak *0x249ff0
commands
silent
printf "SVC32 pc=0x249ff0 r0=0x%x\n", $r0
continue
end
tbreak *0x24a028
commands
silent
printf "SVC32 pc=0x24a028 r0=0x%x\n", $r0
continue
end
tbreak *0x24a090
commands
silent
printf "SVC32 pc=0x24a090 r0=0x%x\n", $r0
continue
end
tbreak *0x24a0d4
commands
silent
printf "SVC32 pc=0x24a0d4 r0=0x%x\n", $r0
continue
end
tbreak *0x24a12c
commands
silent
printf "SVC32 pc=0x24a12c r0=0x%x\n", $r0
continue
end
tbreak *0x2c7530
commands
silent
printf "SVC32 pc=0x2c7530 r0=0x%x\n", $r0
continue
end
tbreak *0x2c7568
commands
silent
printf "SVC32 pc=0x2c7568 r0=0x%x\n", $r0
continue
end
tbreak *0x305f84
commands
silent
printf "SVC32 pc=0x305f84 r0=0x%x\n", $r0
continue
end
tbreak *0x305fd8
commands
silent
printf "SVC32 pc=0x305fd8 r0=0x%x\n", $r0
continue
end
tbreak *0x31086c
commands
silent
printf "SVC32 pc=0x31086c r0=0x%x\n", $r0
continue
end
tbreak *0x31a64c
commands
silent
printf "SVC32 pc=0x31a64c r0=0x%x\n", $r0
continue
end
tbreak *0x31a740
commands
silent
printf "SVC32 pc=0x31a740 r0=0x%x\n", $r0
continue
end
tbreak *0x31a794
commands
silent
printf "SVC32 pc=0x31a794 r0=0x%x\n", $r0
continue
end
tbreak *0x31ac04
commands
silent
printf "SVC32 pc=0x31ac04 r0=0x%x\n", $r0
continue
end
tbreak *0x31b3fc
commands
silent
printf "SVC32 pc=0x31b3fc r0=0x%x\n", $r0
continue
end
tbreak *0x32eaa8
commands
silent
printf "SVC32 pc=0x32eaa8 r0=0x%x\n", $r0
continue
end
continue
