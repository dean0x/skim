## Configuration

The tool accepts a JSON configuration file. Example:

```json
{
  "provider": "anthropic",
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 4096
}
```

Save this as `config.json` and pass it with `--config config.json`.