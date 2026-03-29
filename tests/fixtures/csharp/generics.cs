/**
 * FIXTURE: C# generics and advanced types
 * TESTS: Generic type extraction, constraints
 */

using System;
using System.Collections.Generic;
using System.Linq;

namespace MyApp.Collections
{
    public class Repository<T> where T : class, new()
    {
        private readonly List<T> _items = new();

        public T FindById(int id)
        {
            return _items.FirstOrDefault();
        }

        public void Add(T item)
        {
            _items.Add(item);
        }

        public IEnumerable<T> GetAll()
        {
            return _items.AsReadOnly();
        }

        public int Count()
        {
            return _items.Count;
        }
    }

    public static class Extensions
    {
        public static T OrDefault<T>(T value, T defaultValue)
        {
            return value ?? defaultValue;
        }

        public static List<T> ToSorted<T>(List<T> list) where T : IComparable<T>
        {
            var sorted = new List<T>(list);
            sorted.Sort();
            return sorted;
        }
    }
}
