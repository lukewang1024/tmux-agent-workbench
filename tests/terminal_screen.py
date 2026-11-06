"""Project captured terminal cursor writes for real tmux PTY tests."""
import re
import unicodedata


def screen_rows(output, columns, rows, compact=False):
    screen = {}
    x = y = 0
    tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[ -/]*[0-~]|[^\x1b]', output.decode(errors='replace'))
    for token in tokens:
        if token.startswith('\x1b['):
            command = token[-1]
            values = token[2:-1]
            if any(char not in '0123456789;' for char in values):
                continue
            numbers = [int(value or 0) for value in values.split(';')]
            amount = numbers[0] or 1
            if command in ('H', 'f'):
                y = amount - 1
                x = (numbers[1] or 1) - 1 if len(numbers) > 1 else 0
            elif command == 'G': x = amount - 1
            elif command == 'd': y = amount - 1
            elif command == 'C': x += amount
            elif command == 'D': x -= amount
            elif command == 'A': y -= amount
            elif command == 'B': y += amount
            elif command in ('J', 'K', 'X'):
                mode = numbers[0]
                for col, row in list(screen):
                    if command == 'J':
                        erase = mode == 2 or (mode == 0 and (row, col) >= (y, x)) or (mode == 1 and (row, col) <= (y, x))
                    elif command == 'K':
                        erase = row == y and (mode == 2 or (mode == 0 and col >= x) or (mode == 1 and col <= x))
                    else:
                        erase = row == y and x <= col < x + amount
                    if erase:
                        del screen[col, row]
        elif token.startswith('\x1b'):
            continue
        elif token == '\r': x = 0
        elif token == '\n': y += 1
        elif (token.isprintable() or unicodedata.category(token) == 'Co') and not unicodedata.combining(token):
            screen[x, y] = token
            width = 2 if unicodedata.east_asian_width(token) in ('W', 'F') else 1
            if width == 2:
                screen[x + 1, y] = ''
            x += width
    return [''.join(screen.get((col, row), ' ') or ('' if compact else ' ')
                    for col in range(columns)) for row in range(rows)]

