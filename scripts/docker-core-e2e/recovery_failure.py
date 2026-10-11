import subprocess, time
r=subprocess.run(['treer','agent','startup','set','--cwd','.','--','/does-not-exist/treer-e2e'],capture_output=True,text=True)
print('STARTUP_BAD '+str(r.returncode),flush=True)
time.sleep(3600)
