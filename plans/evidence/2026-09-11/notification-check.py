# Archived, pseudonymized driver; changes live state. See README.md; do not run on an existing installation.
"""Run with: dbus-run-session -- /usr/bin/python3 notification-check.py /path/to/clix.
Exercises production notify-rust against a private D-Bus recorder, not a desktop.
"""
import json, os, pathlib, socket, subprocess, sys, tempfile, threading, time
import dbus, dbus.service, dbus.mainloop.glib
from gi.repository import GLib
BIN=sys.argv[1]
calls=[]
dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus=dbus.SessionBus()
name=dbus.service.BusName('org.freedesktop.Notifications',bus=bus)
class Notifications(dbus.service.Object):
    @dbus.service.method('org.freedesktop.Notifications',in_signature='',out_signature='as')
    def GetCapabilities(self): return ['actions','body','persistence']
    @dbus.service.method('org.freedesktop.Notifications',in_signature='',out_signature='ssss')
    def GetServerInformation(self): return ('Clix test recorder','test','1','1.2')
    @dbus.service.method('org.freedesktop.Notifications',in_signature='susssasa{sv}i',out_signature='u')
    def Notify(self,app,replaces,icon,summary,body,actions,hints,timeout):
        calls.append({'body':str(body),'actions':[str(x) for x in actions]}); return len(calls)
    @dbus.service.method('org.freedesktop.Notifications',in_signature='u',out_signature='')
    def CloseNotification(self,id): pass
    @dbus.service.signal('org.freedesktop.Notifications',signature='us')
    def ActionInvoked(self,id,action): pass
recorder=Notifications(bus,'/org/freedesktop/Notifications')
threading.Thread(target=GLib.MainLoop().run,daemon=True).start()
def rpc(sock,req):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(5); s.connect(str(sock)); s.sendall((json.dumps(req)+'\n').encode())
        result=json.loads(s.makefile('rb').readline())
        assert result.get('ok') is not False,result
        return result

def eventually(check):
    for _ in range(300):
        if check(): return
        time.sleep(.02)
    raise AssertionError('timed out')

def start(tmp,enabled):
    env=os.environ.copy(); env.update(CLIX_HOME=str(tmp/'state'),CLIX_SOCK=str(tmp/'sock'),CLIX_PIN=str(tmp/'src'),CLIX_MESH_BIND='127.0.0.1:0',CLIX_NOTIFY='1' if enabled else '0',CLIX_TRAY='0',DISPLAY=':test')
    p=subprocess.Popen([BIN,'daemon'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    def ready():
        assert p.poll() is None,p.communicate()
        try: rpc(tmp/'sock',{'op':'status'}); return True
        except OSError: return False
    eventually(ready); return p

with tempfile.TemporaryDirectory(prefix='clix-notification-check-') as d:
    tmp=pathlib.Path(d); p=start(tmp,False)
    try:
        body=rpc(tmp/'sock',{'op':'status'})['body']
        rpc(tmp/'sock',{'op':'request','body':body,'tool':'true'})
        assert not calls
    finally: p.terminate(); p.communicate(timeout=5)
    p=start(tmp,True)
    try:
        eventually(lambda:len(calls)==1)
        assert len(rpc(tmp/'sock',{'op':'pending'})['requests'])==1
        assert calls[0]['actions']==['default','Allow once','once','Allow once','allow','Allow','deny','Deny']
        print('Persisted pending request replayed through production notify-rust:',calls[0])
        recorder.ActionInvoked(1,'once')
        eventually(lambda:len(rpc(tmp/'sock',{'op':'hands'})['hands'])==1)
        grant=rpc(tmp/'sock',{'op':'hands'})['hands'][0]
        assert grant['once'] and grant['allow_from']==[body],grant
        assert not rpc(tmp/'sock',{'op':'pending'})['requests']
        print('D-Bus Allow once action updated the same persisted grant and pending objects.')
    finally: p.terminate(); p.communicate(timeout=5)
