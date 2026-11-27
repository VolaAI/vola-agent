import { mount } from "nsh";
import { Window } from "nsh/desktop";
import { App } from "./src/ui";

mount(() => (
  <Window id="main">
    <App />
  </Window>
));
