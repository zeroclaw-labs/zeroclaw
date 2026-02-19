#!/usr/bin/env python3
"""
PTY helper for ClawSuite terminal.
Spawns a shell in a real PTY and bridges stdin/stdout.
Usage: python3 pty-helper.py [shell] [cwd] [cols] [rows]
"""
import sys, os, pty, select, signal, struct, fcntl, termios

def set_winsize(fd, rows, cols):
    s = struct.pack('HHHH', rows, cols, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, s)

def main():
    default_shell = '/bin/zsh' if sys.platform == 'darwin' else '/bin/bash'
    shell = sys.argv[1] if len(sys.argv) > 1 else os.environ.get('SHELL', default_shell)
    cwd = sys.argv[2] if len(sys.argv) > 2 else os.environ.get('HOME', '/tmp')
    cols = int(sys.argv[3]) if len(sys.argv) > 3 else 80
    rows = int(sys.argv[4]) if len(sys.argv) > 4 else 24

    if cwd.startswith('~'):
        cwd = os.path.expanduser(cwd)

    # Create PTY
    master_fd, slave_fd = pty.openpty()
    set_winsize(master_fd, rows, cols)

    pid = os.fork()
    if pid == 0:
        # Child: become session leader, set controlling terminal
        os.setsid()
        os.close(master_fd)

        # Set slave as controlling terminal
        fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)

        os.dup2(slave_fd, 0)
        os.dup2(slave_fd, 1)
        os.dup2(slave_fd, 2)
        if slave_fd > 2:
            os.close(slave_fd)

        os.chdir(cwd)
        os.environ['TERM'] = 'xterm-256color'
        os.environ['COLORTERM'] = 'truecolor'
        os.execvp(shell, [shell, '-i'])
    else:
        # Parent: bridge stdin <-> master_fd <-> stdout
        os.close(slave_fd)

        # Make stdin non-blocking
        import io
        stdin_fd = sys.stdin.fileno()
        stdout_fd = sys.stdout.fileno()

        # Set stdout to binary/unbuffered
        sys.stdout = io.TextIOWrapper(sys.stdout.buffer, write_through=True)

        # Handle resize signal
        def handle_winch(signum, frame):
            # Read new size from environment (set by parent process)
            try:
                new_cols = int(os.environ.get('COLUMNS', cols))
                new_rows = int(os.environ.get('LINES', rows))
                set_winsize(master_fd, new_rows, new_cols)
                os.kill(pid, signal.SIGWINCH)
            except:
                pass

        signal.signal(signal.SIGWINCH, handle_winch)

        try:
            while True:
                rlist, _, _ = select.select([master_fd, stdin_fd], [], [], 1.0)
                
                if master_fd in rlist:
                    try:
                        data = os.read(master_fd, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    os.write(stdout_fd, data)

                if stdin_fd in rlist:
                    try:
                        data = os.read(stdin_fd, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    os.write(master_fd, data)
        except (IOError, OSError):
            pass
        finally:
            os.close(master_fd)
            try:
                os.kill(pid, signal.SIGTERM)
                os.waitpid(pid, 0)
            except:
                pass

if __name__ == '__main__':
    main()
