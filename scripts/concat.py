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

if '--client' in sys.argv:
    files.append('client/src-tauri/capabilities/default.json')
    files.append('client/src-tauri/build.rs')
    files.append('client/src-tauri/Cargo.toml')
    files.append('client/src-tauri/tauri.conf.json')
    files.append('client/src-tauri/src/commands.rs')
    files.append('client/src-tauri/src/crypto.rs')
    files.append('client/src-tauri/src/db.rs')
    files.append('client/src-tauri/src/main.rs')
    files.append('client/src-tauri/src/protocol.rs')
    files.append('client/src-tauri/src/state.rs')
    files.append('client/src-tauri/src/util.rs')
    files.append('client/src-tauri/src/ws.rs')
    files.append('client/src/index.html')
    files.append('client/src/css/app.css')
    files.append('client/src/css/base.css')
    files.append('client/src/css/login.css')
    files.append('client/src/js/actions.js')
    files.append('client/src/js/api.js')
    files.append('client/src/js/dialog.js')
    files.append('client/src/js/encryption.js')
    files.append('client/src/js/events.js')
    files.append('client/src/js/main.js')
    files.append('client/src/js/sound.js')
    files.append('client/src/js/state.js')
    files.append('client/src/js/theme.js')
    files.append('client/src/js/ui.js')
    files.append('client/src/js/utils.js')

out = open('joined.txt', 'w', encoding='utf-8')
ret = ''

for fn in files:
    ret += 'FILE: ' + fn + '\n'
    for line in open(os.path.join(cwd, fn), encoding='utf-8').read().split('\n'):
        ret += line.strip() + '\n'

out.write(ret)
