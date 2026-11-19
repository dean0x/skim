import { useState } from "react";

// The banner sits above every page. Its markup is JSX, which the plain
// TypeScript grammar reads as a type assertion.
const banner = <div className="banner">Welcome</div>;

interface ButtonProps {
  label: string;
  onClick: () => void;
}

export function Button({ label, onClick }: ButtonProps): JSX.Element {
  // Track the press so the style can change.
  const [pressed, setPressed] = useState(false);
  return (
    <button className={pressed ? "pressed" : "idle"} onClick={() => setPressed(true)}>
      {label}
    </button>
  );
}

// An item is one row of the list. The id is the key of the row, so two items
// must not share an id.
type Item = { id: number; name: string };

// The list shows the items in the order the caller gives them. It does not
// sort them, and it does not drop an item with an empty name.
export function ItemList({ items }: { items: Item[] }): JSX.Element {
  // Render one row for each item.
  return (
    <ul>
      {items.map((item) => (
        <li key={item.id}>{item.name}</li>
      ))}
    </ul>
  );
}

// The panel puts its title in a heading above the banner. The title is fixed
// when the panel is made.
export class Panel {
  constructor(private readonly title: string) {}

  render(): JSX.Element {
    // The title goes in a heading.
    return <section><h2>{this.title}</h2>{banner}</section>;
  }
}

// The footer closes every page. It has no state and takes no props.
export function Footer(): JSX.Element {
  return <footer>done</footer>;
}
