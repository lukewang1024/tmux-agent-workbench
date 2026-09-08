# Work from the captured layout, never the pre-sidebar layout cache. POSIX awk.
function fail() { print "workbench: invalid resurrect layout" > "/dev/stderr"; exit 1 }
function number(    value) {
  if (!match(input, /^[0-9]+/)) fail()
  value = substr(input, 1, RLENGTH) + 0
  input = substr(input, RLENGTH + 1)
  return value
}
function consume(s) {
  if (substr(input, 1, 1) != s) fail()
  input = substr(input, 2)
}
function parse(    n, c, closing) {
  n = ++serial
  width[n] = number(); consume(",")
  height[n] = number(); consume(",")
  xpos[n] = number(); consume(",")
  ypos[n] = number()
  kind[n] = substr(input, 1, 1)
  if (kind[n] == ",") {
    consume(","); pane[n] = number()
    return n
  }
  if (kind[n] != "[" && kind[n] != "{") fail()
  closing = kind[n] == "[" ? "]" : "}"
  consume(kind[n])
  do {
    c = parse()
    child[n, ++count[n]] = c
    if (substr(input, 1, 1) == closing) break
    consume(",")
  } while (1)
  consume(closing)
  return n
}
# Removing leaves never shrinks the available rectangle. Distribute the extra
# space proportionally, retaining split orientation and main-pane proportions.
function fit(n, w, h, x, y,    i, c, old, extra, used, add, size, total, offset) {
  width[n] = w; height[n] = h; xpos[n] = x; ypos[n] = y
  if (kind[n] == ",") return
  total = 0
  for (i = 1; i <= count[n]; i++) {
    c = child[n, i]
    total += kind[n] == "{" ? width[c] : height[c]
  }
  extra = (kind[n] == "{" ? w : h) - (count[n] - 1) - total
  if (extra < 0 || total < 1) fail()
  used = 0; old = 0; offset = 0
  for (i = 1; i <= count[n]; i++) {
    c = child[n, i]
    size = kind[n] == "{" ? width[c] : height[c]
    old += size
    add = int(extra * old / total) - used
    used += add; size += add
    if (kind[n] == "{") fit(c, size, h, x + offset, y)
    else fit(c, w, size, x, y + offset)
    offset += size + 1
  }
}
function prune(n,    i, c, kept) {
  if (kind[n] == ",") return (window_key SUBSEP pane[n]) in removed ? 0 : n
  kept = 0
  for (i = 1; i <= count[n]; i++) {
    c = prune(child[n, i])
    if (c) child[n, ++kept] = c
  }
  count[n] = kept
  if (!kept) return 0
  if (kept == 1) {
    c = child[n, 1]
    fit(c, width[n], height[n], xpos[n], ypos[n])
    return c
  }
  fit(n, width[n], height[n], xpos[n], ypos[n])
  return n
}
function render(n,    s, i) {
  s = width[n] "x" height[n] "," xpos[n] "," ypos[n]
  if (kind[n] == ",") return s "," pane[n]
  s = s kind[n]
  for (i = 1; i <= count[n]; i++) s = s (i > 1 ? "," : "") render(child[n, i])
  return s (kind[n] == "{" ? "}" : "]")
}
function checksum(s,    i, sum) {
  sum = 0
  for (i = 1; i <= length(s); i++) {
    sum = int(sum / 2) + (sum % 2) * 32768
    sum = (sum + ascii[substr(s, i, 1)]) % 65536
  }
  return sprintf("%04x", sum)
}
BEGIN { for (code = 32; code < 127; code++) ascii[sprintf("%c", code)] = code }
FILENAME == ARGV[1] {
  sidebar[$1, $2, $3] = 1
  removed[$1, $2, substr($4, 2)] = 1
  affected[$1, $2] = 1
  next
}
$1 == "pane" && (($2 SUBSEP $3 SUBSEP $6) in sidebar) { next }
$1 == "window" && (($2 SUBSEP $3) in affected) {
  window_key = $2 SUBSEP $3
  input = substr($7, 6)
  # Convert the dimension separator for the numeric parser at every node.
  gsub(/x/, ",", input)
  root = parse()
  if (input != "") fail()
  root = prune(root)
  if (!root) fail()
  layout = render(root)
  $7 = checksum(layout) "," layout
}
{ print }
