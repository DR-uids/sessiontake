import type { Page } from "./transcript";

type FeedOptions<T> = {
  read: (page: number) => Promise<Page<T>>;
  append: (content: T) => void;
  loading: () => string;
  error: (error: unknown) => string;
  retry: () => string;
};

export class ScrollFeed<T> {
  private readonly tail = document.createElement("div");
  private readonly observer: IntersectionObserver;
  private page = 0;
  private total = Infinity;
  private pending = false;
  private failure?: { error: unknown };
  private disposed = false;
  private top = 0;
  private left = 0;

  constructor(readonly element: HTMLElement, private readonly options: FeedOptions<T>) {
    this.tail.className = "feed-tail";
    this.tail.setAttribute("role", "status");
    element.append(this.tail);
    this.observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void this.load();
    }, { root: element, rootMargin: "0px 0px 180px" });
  }

  mount(host: HTMLElement) {
    host.append(this.element);
    this.element.scrollTop = this.top;
    this.element.scrollLeft = this.left;
    this.observe();
    if (this.page === 0) void this.load();
  }

  unmount() {
    this.top = this.element.scrollTop;
    this.left = this.element.scrollLeft;
    this.observer.disconnect();
    this.element.remove();
  }

  dispose() {
    this.disposed = true;
    this.observer.disconnect();
  }

  observe() {
    this.observer.disconnect();
    if (!this.disposed && this.element.isConnected && !this.failure && this.page < this.total) {
      this.observer.observe(this.tail);
    }
  }

  refreshStatus() {
    this.tail.replaceChildren();
    if (this.pending) {
      const spinner = document.createElement("span");
      spinner.className = "spinner";
      spinner.setAttribute("aria-hidden", "true");
      this.tail.append(spinner, this.options.loading());
    } else if (this.failure) {
      const retry = document.createElement("button");
      retry.className = "ghost";
      retry.type = "button";
      retry.textContent = this.options.retry();
      retry.addEventListener("click", () => {
        this.element.focus({ preventScroll: true });
        this.failure = undefined;
        void this.load();
      });
      this.tail.append(this.options.error(this.failure.error), retry);
    }
  }

  private async load() {
    if (this.disposed || !this.element.isConnected || this.pending || this.failure || this.page >= this.total) return;
    this.pending = true;
    this.refreshStatus();
    try {
      const data = await this.options.read(this.page);
      if (this.disposed) return;
      this.options.append(data.content);
      this.total = data.total;
      this.page++;
    } catch (error) {
      if (this.disposed) return;
      this.failure = { error };
    } finally {
      this.pending = false;
      if (!this.disposed) {
        this.refreshStatus();
        // Recheck the tail when one batch is too short to fill the viewport.
        this.observe();
      }
    }
  }
}
