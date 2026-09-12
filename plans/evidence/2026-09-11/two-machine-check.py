# Archived, pseudonymized driver; changes live state. See README.md; do not run on an existing installation.
import json, os, pathlib, shlex, socket, subprocess, time
BIN='/home/owner/.local/bin/clix'

def run(args, *, expected=0, timeout=40):
    p=subprocess.run(args,capture_output=True,text=True,timeout=timeout)
    print('$',shlex.join(args),flush=True)
    print(p.stdout,end='',flush=True)
    print(p.stderr,end='',flush=True)
    print('exit:',p.returncode,flush=True)
    assert p.returncode == expected,(args,p.returncode,p.stderr)
    return p.stdout

def remote(*args,expected=0):
    return run(['ssh','server',shlex.join([BIN,*args])],expected=expected)

def rpc(req):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(40)
        s.connect('/run/user/1000/clix.sock')
        s.sendall((json.dumps(req)+'\n').encode())
        return json.loads(s.makefile('rb').readline())

assert rpc({'op':'status'})['peers'] == [],'proof requires fresh pair; do not replace existing identity'
pair=subprocess.Popen([BIN,'pair'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
line=pair.stdout.readline().strip()
assert line.startswith('pair with: '),line
phrase=line.removeprefix('pair with: ')
p=subprocess.run(['ssh','server',shlex.join([BIN,'pair',phrase])],capture_output=True,text=True,timeout=90)
print('pair: laptop CLI phrase -> server CLI phrase (phrase omitted)',flush=True)
print('join exit:',p.returncode,p.stdout,p.stderr,flush=True)
assert p.returncode == 0,p.stderr
out,err=pair.communicate(timeout=45)
print('listen exit:',pair.returncode,out,err,flush=True)
assert pair.returncode == 0,err
run([BIN,'status']); remote('status')
remote('laptop','adb','devices',expected=1)
assert not rpc({'op':'hands'})['hands']
run([BIN,'pending']); run([BIN,'deny'])
run([BIN,'add','adb','--allow','server','--once'])
remote('laptop','adb','devices')
assert not rpc({'op':'hands'})['hands'],'successful once grant was not consumed'
print('once grant consumed on laptop',flush=True)
remote('laptop','bash',expected=1)
run([BIN,'pending']); run([BIN,'deny'])
run([BIN,'add','adb','--allow','server'])
remote('laptop','adb','devices')
# A sidecar outage across two real hosts, not a physical lid-sleep test.
run(['systemctl','--user','stop','clix.service'])
try:
    job_id=remote('--no-wait','laptop','adb','devices').strip()
    assert len(job_id)==36,job_id
    remote('request','laptop','true')
    run(['ssh','server','systemctl --user restart clix.service'])
finally:
    run(['systemctl','--user','start','clix.service'])
for _ in range(100):
    try:
        rows=rpc({'op':'log'})['jobs']
        job=next((j for j in rows if j['id']==job_id),None)
        if job and job['status']=={'Done':{'exit':0}}: break
    except OSError: pass
    time.sleep(.2)
else: raise AssertionError('waiting job did not finish')
print('recovered waiting job:',json.dumps(job),flush=True)
for _ in range(100):
    pending=rpc({'op':'pending'})['requests']
    if any(r['tool']=='true' and r['from']=='server' for r in pending):break
    time.sleep(.2)
else:raise AssertionError('offline permission request was not delivered')
assert len(pending)==1,pending
run([BIN,'pending']);run([BIN,'deny'])
local=rpc({'op':'log'})['jobs']
raw=run(['ssh','server',"python3 -c 'import json,socket; s=socket.socket(socket.AF_UNIX); s.connect(\"/run/user/1000/clix.sock\"); s.sendall(b\"{\\\"op\\\":\\\"log\\\"}\\n\"); print(s.makefile().readline())'"])
origin=json.loads(raw)['jobs']
assert {j['id']:j for j in local}=={j['id']:j for j in origin},'logs differ'
print('matching durable jobs on both boxes:',len(local),flush=True)
run([BIN,'log']);remote('log')
print('remaining grants:',json.dumps(rpc({'op':'hands'})['hands']),flush=True)
assert len(rpc({'op':'hands'})['hands'])==1
print('TWO-MACHINE PROOF PASSED; physical lid-sleep and desktop clicks not tested',flush=True)
