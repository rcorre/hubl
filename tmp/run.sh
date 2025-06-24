args="$(cat $1)"

jq '{"query": $query, "variables": .}' --rawfile query $1 $2 \
  | curl -s -H 'Accept: application/vnd.github.text-match+json' -H "Authorization: bearer $(gh auth token)" -X POST -d @- https://api.github.com/graphql \
  | jq .
