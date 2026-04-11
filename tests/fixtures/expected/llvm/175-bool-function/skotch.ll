; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.true = private unnamed_addr constant [5 x i8] c"true\00", align 1
@.str.false = private unnamed_addr constant [6 x i8] c"false\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @InputKt_isAdult(i32 %arg0) {
entry:
  %t0 = add i32 0, 18
  %t1 = icmp sge i32 %arg0, %t0
  %t2 = zext i1 %t1 to i32
  ret i32 %t2
}

define i32 @InputKt_isTeenager(i32 %arg0) {
entry:
  %merge_1 = alloca i32
  %t0 = add i32 0, 13
  %t1 = icmp sge i32 %arg0, %t0
  store i32 0, ptr %merge_1
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 18
  %t3 = icmp slt i32 %arg0, %t2
  %t4 = zext i1 %t3 to i32
  store i32 %t4, ptr %merge_1
  br label %bb2
bb2:
  %t5 = load i32, ptr %merge_1
  ret i32 %t5
}

define i32 @main() {
entry:
  %t0 = add i32 0, 20
  %t1 = call i32 @InputKt_isAdult(i32 %t0)
  %t3 = trunc i32 %t1 to i1
  %t4 = select i1 %t3, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 10
  %t7 = call i32 @InputKt_isAdult(i32 %t6)
  %t9 = trunc i32 %t7 to i1
  %t10 = select i1 %t9, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 15
  %t13 = call i32 @InputKt_isTeenager(i32 %t12)
  %t15 = trunc i32 %t13 to i1
  %t16 = select i1 %t15, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t16)
  %t18 = add i32 0, 20
  %t19 = call i32 @InputKt_isTeenager(i32 %t18)
  %t21 = trunc i32 %t19 to i1
  %t22 = select i1 %t21, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t22)
  ret i32 0
}

