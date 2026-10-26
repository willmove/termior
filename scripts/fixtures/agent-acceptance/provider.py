import argparse, json, pathlib, uuid, re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser(description='Loopback-only deterministic model fixture for desktop acceptance.')
parser.add_argument('--artifacts', type=pathlib.Path, required=True)
parser.add_argument('--port', type=int, default=0)
args = parser.parse_args()
root = args.artifacts.resolve()
root.mkdir(parents=True, exist_ok=True)
class Provider(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with (root/'provider-requests.jsonl').open('a', encoding='utf-8') as f:
            f.write(json.dumps(request, ensure_ascii=False)+'\n')
        messages = request['messages']
        user_index = max(i for i,m in enumerate(messages) if m['role']=='user')
        case = str(messages[user_index]['content'])
        results = [m['content'] for m in messages[user_index+1:] if m['role']=='tool']
        name, args, text = None, None, None
        if not results:
            if 'CASE_READ' in case: name,args='read_file',{'path':'acceptance.txt'}
            elif 'CASE_BUILD' in case: name,args='run_command',{'command':'pwsh -NoProfile -File slow-build.ps1'}
            elif 'CASE_RECOVERY' in case: name,args='run_command',{'command':'pwsh -NoProfile -Command "Set-Content recovery-forbidden.txt forbidden"'}
            elif 'CASE_EDIT' in case: name,args='write_file',{'path':'editable.txt','content':'agent edited content\n'}
            elif 'CASE_REPL' in case: name,args='command_claim',{'session_id':'cmd-1'}
            elif 'CASE_CANCEL' in case: name,args='run_command',{'command':'pwsh -NoProfile -File cancel.ps1'}
            elif 'CASE_APPROVE' in case: name,args='run_command',{'command':'pwsh -NoProfile -Command "Set-Content approved.txt approved"'}
            elif 'CASE_DENY' in case: name,args='run_command',{'command':'pwsh -NoProfile -Command "Set-Content denied-forbidden.txt forbidden"'}
            elif 'CASE_BACKGROUND' in case: name,args='shell_bg_spawn',{'command':'pwsh -NoProfile -File background.ps1'}
            elif 'CASE_STOP_BACKGROUND' in case:
                found=re.findall(r'session_id=(cmd-\d+) state=running', json.dumps(messages))
                name,args='command_kill',{'session_id':found[-1]}
            else: text='FIXTURE_READY'
        else:
            if 'CASE_REPL' in case and len(results)==1:
                name,args='command_write_input',{'session_id':'cmd-1','data':"Write-Output 'agent-repl-proof'\r"}
            else: text='FIXTURE_RESULT '+str(results[-1])
        if name:
            delta={'tool_calls':[{'index':0,'id':'call-acceptance-'+uuid.uuid4().hex[:8], 'type':'function', 'function':{'name':name,'arguments':json.dumps(args)}}]}
            reason='tool_calls'
        else:
            delta={'content':text}; reason='stop'
        payload='data: '+json.dumps({'id':'acceptance','object':'chat.completion.chunk','choices':[{'index':0,'delta':delta,'finish_reason':None}]})+'\n\n'
        payload+='data: '+json.dumps({'id':'acceptance','choices':[{'index':0,'delta':{},'finish_reason':reason}]})+'\n\ndata: [DONE]\n\n'
        self.send_response(200)
        self.send_header('Content-Type','text/event-stream')
        self.send_header('Content-Length',str(len(payload.encode())))
        self.end_headers()
        self.wfile.write(payload.encode())

server=ThreadingHTTPServer(('127.0.0.1',args.port),Provider)
(root/'provider.port').write_text(str(server.server_port))
server.serve_forever()
