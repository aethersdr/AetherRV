// Pages not ported yet: the right title (in the top bar) and a note. Page agents replace these.
import { Icon } from "../components/Icon";
import { VIEWS, type ViewId } from "../state/route";

export function Placeholder({ view }: { view: ViewId }) {
  const [, title, ic] = VIEWS.find(v => v[0] === view)!;
  return (
    <div className="empty-state placeholder" data-placeholder={view}>
      <Icon name={ic} />
      <div>{title}: not ported to the React UI yet.</div>
    </div>
  );
}
