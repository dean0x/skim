@Injectable({ providedIn: "root" })
@Component({ selector: "app-user", template: "<div></div>" })
export class UserService {
  readonly cache: Map<string, string> = new Map();
}
export interface User {
  id: UserId;
  email: string;
}
