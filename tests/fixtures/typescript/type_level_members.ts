// Billing domain types and the settlement service that consumes them.

type UserId = string;
declare function Injectable(o: object): ClassDecorator;
declare function Component(o: object): ClassDecorator;
declare function Directive(o: object): ClassDecorator;
declare function Pipe(o: object): ClassDecorator;

export type Money =
  | { amount: number; currency: "USD" }
  | { amount: number; currency: "EUR" };

export interface User {
  id: UserId;
  email: string;
  readonly createdAt: Date;
}

export interface Ledger {
  [account: string]: number;
}

@Injectable({ providedIn: "root" })
@Component({ selector: "app-settlement", template: "<div></div>" })
@Directive({ selector: "[appSettlementHost]", exportAs: "settlement" })
@Pipe({ name: "settlementFormat", pure: true, standalone: true })
@Directive({ selector: "[appSettlementAudit]", exportAs: "settlementAudit" })
export class SettlementService {
  private readonly cache: Map<string, Money> = new Map();

  settle(a: Money, b: Money): Money {
    const total = a.amount + b.amount;
    return { amount: total, currency: a.currency };
  }
}
