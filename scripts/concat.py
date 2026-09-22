import os
import sys


cwd = os.path.dirname(os.path.dirname(__file__))
files = []

if '--server' in sys.argv:
    files.append('server/Cargo.toml')
    files.append('server/src/auth.rs')
    files.append('server/src/db.rs')
    files.append('server/src/main.rs')
    files.append('server/src/protocol.rs')
    files.append('server/src/state.rs')
    files.append('server/src/util.rs')
    files.append('server/src/ws.rs')

out = open('joined.txt', 'w', encoding='utf-8')
ret = ''

for fn in files:
    ret += 'FILE: ' + fn + '\n'
    for line in open(os.path.join(cwd, fn), encoding='utf-8').read().split('\n'):
        ret += line.strip() + '\n'

out.write(ret)
