from dataclasses import asdict, dataclass, field


@dataclass(frozen=True)
class Word:
    start: float
    end: float
    text: str


@dataclass
class Transcript:
    language: str
    duration: float
    words: list[Word]


@dataclass
class Sentence:
    id: int
    start: float
    end: float
    text: str
    words: list[Word] = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> "Sentence":
        words = [Word(**w) for w in data.get("words", [])]
        return cls(id=data["id"], start=data["start"], end=data["end"], text=data["text"], words=words)
