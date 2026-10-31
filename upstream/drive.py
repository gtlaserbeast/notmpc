# Runs a notcurses program in a pty (answering its startup queries),
# sends SIGWINCH at 2 s and 4 s (second one with a real size change),
# and q at 7 s.
import os, pty, sys, time, select, fcntl, termios, struct, signal, re
argv = sys.argv[1:]
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = os.environ.get("DRIVE_TERM", "xterm-256color"); os.execv(argv[0], argv)
CH, CW = 22, 10
def size(r, c): fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", r, c, c*CW, r*CH))
size(40, 120); cur = (40, 120)
out = bytearray(); T0 = time.time(); ev = []
def pump(t):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.02)
        if not r: continue
        try: d = os.read(fd, 1 << 20)
        except OSError: return
        for m in re.finditer(rb"\x1b\[2J|\x1b_Gf=32,s=\d+,v=\d+|\x1b_Ga=p,i=\d+|\x1b_Ga=d[^\x1b]*", d): ev.append(f"{time.time()-T0:5.2f} out {m.group(0)[1:].decode()}")
        out.extend(d)
        if b"\x1b[6n" in d: os.write(fd, b"\x1b[1;1R")
        if b"\x1b[>0q" in d: os.write(fd, b"\x1bP>|kitty(0.39.1)\x1b\\")
        for m in re.finditer(rb"\x1b_Gi=(\d+),[^;]*a=q[^;]*;", d): os.write(fd, b"\x1b_Gi=" + m.group(1) + b";OK\x1b\\")
        if b"\x1b[c" in d or b"\x1b[0c" in d: os.write(fd, b"\x1b[?62;22;52c")
        if b"\x1b[16t" in d: os.write(fd, f"\x1b[6;{CH};{CW}t".encode())
        if b"\x1b[14t" in d: os.write(fd, f"\x1b[4;{cur[0]*CH};{cur[1]*CW}t".encode())
pump(2)
ev.append(f"{time.time()-T0:5.2f} SIGWINCH (same size)"); size(40, 120); os.kill(pid, signal.SIGWINCH); pump(2)
ev.append(f"{time.time()-T0:5.2f} SIGWINCH (40x120 -> 35x100)"); cur = (35, 100); size(35, 100); os.kill(pid, signal.SIGWINCH); pump(3)
ev.append(f"{time.time()-T0:5.2f} send q"); os.write(fd, b"q"); pump(1)
print("\n".join(e for e in ev if float(e.split()[0]) > 1.5))
