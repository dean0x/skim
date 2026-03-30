/**
 * FIXTURE: C# interfaces and abstract classes
 * TESTS: Interface/abstract extraction
 */

using System;
using System.Threading.Tasks;

namespace MyApp.Contracts
{
    public interface ILogger
    {
        void Info(string message);
        void Warn(string message);
        void Error(string message, Exception ex);
    }

    public interface IEventHandler<TEvent>
    {
        Task HandleAsync(TEvent ev);
    }

    public abstract class BaseService
    {
        protected readonly ILogger Logger;

        protected BaseService(ILogger logger)
        {
            Logger = logger;
        }

        public abstract Task<bool> ValidateAsync();

        protected virtual void OnCompleted()
        {
            Logger.Info("Operation completed");
        }
    }
}
