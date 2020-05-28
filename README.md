# API costs

Each call is retried up to 5 times, so an unstable network connection can greatly increase API costs.

- fetch labels
  - max(1, n / 100) to fetch the n labels of the repository
- fetch issues
  - max(1, n / 100) to fetch the n issues that have updated since the last synchronisation

# Limitations

- Only fetches first 100 labels per issue, additional labels are ignored
