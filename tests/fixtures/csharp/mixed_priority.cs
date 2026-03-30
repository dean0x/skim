// FIXTURE: C# file with mixed priority items
// TESTS: Truncation priority testing

using System;
using System.Collections.Generic;

namespace MyApp
{
    public enum LogLevel
    {
        Debug,
        Info,
        Warn,
        Error
    }

    public interface IConfigService
    {
        string GetHost();
        int GetPort();
    }

    public class Config
    {
        public string Host { get; set; }
        public int Port { get; set; }
        public int Timeout { get; set; }
    }

    public class Server
    {
        private readonly Config _config;

        public Server(Config config)
        {
            _config = config;
        }

        public void Start()
        {
            Console.WriteLine($"Starting on {_config.Host}:{_config.Port}");
        }

        public void Stop()
        {
            Console.WriteLine("Stopping server");
        }
    }

    public static int ProcessRequest(string url, Config config)
    {
        if (string.IsNullOrEmpty(url) || config == null)
            return -1;
        Console.WriteLine($"Processing: {url} on port {config.Port}");
        return 0;
    }
}
