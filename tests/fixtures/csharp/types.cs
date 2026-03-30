/**
 * FIXTURE: C# type definitions
 * TESTS: Type extraction (interfaces, enums, structs, records)
 */

using System;

namespace MyApp.Models
{
    public interface IRepository<T>
    {
        Task<T> FindById(int id);
        Task<List<T>> FindAll();
        Task Save(T entity);
        Task Delete(int id);
    }

    public enum Status
    {
        Active,
        Inactive,
        Pending,
        Suspended
    }

    public struct Point
    {
        public double X;
        public double Y;

        public Point(double x, double y)
        {
            X = x;
            Y = y;
        }

        public double DistanceTo(Point other)
        {
            double dx = X - other.X;
            double dy = Y - other.Y;
            return Math.Sqrt(dx * dx + dy * dy);
        }
    }

    public class User
    {
        public int Id { get; set; }
        public string Name { get; set; }
        public string Email { get; set; }
        public Status Status { get; set; }
    }
}
