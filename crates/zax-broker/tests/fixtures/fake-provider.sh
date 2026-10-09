#!/bin/sh
# A ticket provider for tests. Behaviour is chosen by $FAKE_MODE:
#   ok        normal answers
#   badproto  answers initialize with protocol 2
#   hang      never answers tickets/assigned
#   die       exits after initialize
#   closeout  closes stdout after initialize but keeps running
#   noauth    answers initialize with a not_authenticated error
# Every tickets/assigned is counted in $FAKE_COUNT if set.
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      if [ "$FAKE_MODE" = noauth ]; then
        printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32001,"message":"not logged in; run `acli jira auth login`","data":{"kind":"not_authenticated"}}}\n' "$id"
        continue
      fi
      proto=1; [ "$FAKE_MODE" = badproto ] && proto=2
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocol":%s,"name":"Fake","capabilities":{"transitions":true},"poll":"1m"}}\n' "$id" "$proto"
      [ "$FAKE_MODE" = die ] && exit 3
      [ "$FAKE_MODE" = closeout ] && { exec 1>&-; sleep 30; } ;;
    *'"tickets/assigned"'*)
      [ -n "$FAKE_COUNT" ] && echo x >> "$FAKE_COUNT"
      [ "$FAKE_MODE" = hang ] && sleep 30
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tickets":[{"key":"FAKE-1","summary":"One","status":{"category":"to_do","name":"To Do"},"url":"https://x/browse/FAKE-1"}]}}\n' "$id" ;;
    *'"tickets/transitions"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"transitions":[{"id":"Done","name":"Done"}]}}\n' "$id" ;;
    *'"shutdown"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"; exit 0 ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"no","data":{"kind":"unsupported"}}}\n' "$id" ;;
  esac
done
