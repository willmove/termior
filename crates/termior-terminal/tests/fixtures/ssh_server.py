"""Loopback-only SSH/SFTP fixture. Requires paramiko in a disposable test venv.

All keys and remote files live under the temp directory passed by the Rust test.
Never run this fixture on a public interface.
"""
import json
import os
from pathlib import Path
import socket
import sys
import threading
import time
import paramiko

root = Path(sys.argv[1]).resolve()
remote = root / "remote"
remote.mkdir()
host_key = paramiko.RSAKey.generate(2048)
user_key = paramiko.RSAKey.generate(2048)
user_key.write_private_key_file(str(root / "identity"))
user_key.write_private_key_file(str(root / "encrypted_identity"), password="fixture-password")

class Server(paramiko.ServerInterface):
    def check_auth_password(self, username, password):
        return paramiko.AUTH_SUCCESSFUL if username == "test" and password == "fixture-password" else paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        return paramiko.AUTH_SUCCESSFUL if username == "test" and key == user_key else paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        if username == "mfa":
            return "keyboard-interactive"
        return "publickey,password"

    def check_auth_interactive(self, username, submethods):
        return paramiko.InteractiveQuery("Test MFA", "", ("Verification code:", False))

    def check_auth_interactive_response(self, responses):
        return paramiko.AUTH_SUCCESSFUL if responses == ["123456"] else paramiko.AUTH_FAILED

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_pty_request(self, *args):
        return True

    def check_channel_window_change_request(self, *args):
        return True

    def check_channel_shell_request(self, channel):
        def shell():
            channel.send(b"fixture-shell-ready\r\n")
            while True:
                data = channel.recv(4096)
                if not data:
                    break
                channel.send(data)
                if b"exit" in data:
                    channel.send_exit_status(0)
                    channel.close()
                    break
        threading.Thread(target=shell, daemon=True).start()
        return True

class Sftp(paramiko.SFTPServerInterface):
    def path(self, name):
        path = (remote / name.lstrip("/")).resolve()
        if not path.is_relative_to(remote):
            raise PermissionError(name)
        return path

    def stat(self, name):
        try:
            return paramiko.SFTPAttributes.from_stat(self.path(name).stat())
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    lstat = stat

    def list_folder(self, name):
        try:
            result = []
            for child in self.path(name).iterdir():
                attrs = paramiko.SFTPAttributes.from_stat(child.stat())
                attrs.filename = child.name
                result.append(attrs)
            return result
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def open(self, name, flags, attr):
        try:
            fd = os.open(self.path(name), flags | getattr(os, "O_BINARY", 0), 0o600)
            mode = "r+b" if flags & os.O_RDWR else "wb" if flags & os.O_WRONLY else "rb"
            file = os.fdopen(fd, mode)
            handle = paramiko.SFTPHandle(flags)
            handle.readfile = file
            handle.writefile = file
            handle.stat = lambda: paramiko.SFTPAttributes.from_stat(os.fstat(file.fileno()))
            return handle
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def mkdir(self, name, attr):
        try:
            self.path(name).mkdir()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def remove(self, name):
        try:
            self.path(name).unlink()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def rmdir(self, name):
        try:
            self.path(name).rmdir()
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

    def rename(self, old, new):
        try:
            self.path(old).rename(self.path(new))
            return paramiko.SFTP_OK
        except OSError as e:
            return paramiko.SFTPServer.convert_errno(e.errno)

listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen()
port = listener.getsockname()[1]
(root / "known_hosts").write_text(f"[127.0.0.1]:{port} {host_key.get_name()} {host_key.get_base64()}\n", encoding="utf-8")
(root / "wrong_hosts").write_text(f"[127.0.0.1]:{port} {user_key.get_name()} {user_key.get_base64()}\n", encoding="utf-8")
print(json.dumps({"port": port}), flush=True)

def serve(client):
    transport = paramiko.Transport(client)
    try:
        transport.add_server_key(host_key)
        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Sftp)
        transport.start_server(server=Server())
        while transport.is_active():
            time.sleep(0.02)
    except (EOFError, OSError, paramiko.SSHException):
        pass
    finally:
        transport.close()

while True:
    client, _ = listener.accept()
    threading.Thread(target=serve, args=(client,), daemon=True).start()
