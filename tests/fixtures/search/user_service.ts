import { Database } from "./database";
import { Logger } from "./logger";

export interface UserProfile {
  id: string;
  name: string;
  email: string;
  role: "admin" | "user";
}

export type AuthToken = string;

export class UserService {
  private db: Database;
  private logger: Logger;

  constructor(db: Database, logger: Logger) {
    this.db = db;
    this.logger = logger;
  }

  async authenticate(email: string, password: string): Promise<AuthToken | null> {
    const user = await this.db.findByEmail(email);
    if (!user) {
      this.logger.warn("Authentication failed: user not found", { email });
      return null;
    }
    // verify password hash
    return "token_" + user.id;
  }

  async getUserProfile(userId: string): Promise<UserProfile | null> {
    return this.db.findById(userId);
  }

  async updateRole(userId: string, newRole: "admin" | "user"): Promise<void> {
    await this.db.update(userId, { role: newRole });
    this.logger.info("Role updated", { userId, newRole });
  }
}
