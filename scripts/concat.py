import os
import sys


cwd = os.path.dirname(os.path.dirname(__file__))
files = []

if '--server' in sys.argv:
    files.append('server/Cargo.toml')
    files.append('server/src/main.rs')

out = open('joined.txt', 'w', encoding='utf-8')
ret = ''

for fn in files:
    ret += 'FILE: ' + fn + '\n'
    for line in open(os.path.join(cwd, fn), encoding='utf-8').read().split('\n'):
        ret += line.strip() + '\n'

out.write(ret)
