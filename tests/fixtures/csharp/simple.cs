/**
 * FIXTURE: Simple C# class
 * TESTS: Basic method signature extraction
 */

using System;
using System.Collections.Generic;

namespace MyApp
{
    public class UserService
    {
        private readonly ILogger _logger;

        public UserService(ILogger logger)
        {
            _logger = logger;
        }

        public async Task<User> GetUser(int id)
        {
            var user = await _repository.FindById(id);
            if (user == null)
                throw new NotFoundException($"User {id} not found");
            return user;
        }

        public void DeleteUser(int id)
        {
            _repository.Delete(id);
            _logger.Info($"Deleted user {id}");
        }
    }
}
