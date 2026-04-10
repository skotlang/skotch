; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.true = private unnamed_addr constant [5 x i8] c"true\00", align 1
@.str.false = private unnamed_addr constant [6 x i8] c"false\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %merge_10 = alloca i32
  %merge_18 = alloca i32
  %merge_26 = alloca i32
  %t0 = add i32 0, 5
  %t1 = add i32 0, 0
  %t2 = icmp sgt i32 %t0, %t1
  store i32 0, ptr %merge_2
  br i1 %t2, label %bb1, label %bb2
bb1:
  %t3 = add i32 0, 10
  %t4 = icmp slt i32 %t0, %t3
  %t5 = zext i1 %t4 to i32
  store i32 %t5, ptr %merge_2
  br label %bb2
bb2:
  %t6 = load i32, ptr %merge_2
  %t8 = trunc i32 %t6 to i1
  %t9 = select i1 %t8, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t9)
  %t11 = add i32 0, 0
  %t12 = icmp sgt i32 %t0, %t11
  store i32 1, ptr %merge_10
  br i1 %t12, label %bb4, label %bb3
bb3:
  %t13 = add i32 0, -10
  %t14 = icmp slt i32 %t0, %t13
  %t15 = zext i1 %t14 to i32
  store i32 %t15, ptr %merge_10
  br label %bb4
bb4:
  %t16 = load i32, ptr %merge_10
  %t18 = trunc i32 %t16 to i1
  %t19 = select i1 %t18, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t19)
  %t21 = add i32 0, 0
  %t22 = icmp slt i32 %t0, %t21
  store i32 0, ptr %merge_18
  br i1 %t22, label %bb5, label %bb6
bb5:
  %t23 = add i32 0, -10
  %t24 = icmp sgt i32 %t0, %t23
  %t25 = zext i1 %t24 to i32
  store i32 %t25, ptr %merge_18
  br label %bb6
bb6:
  %t26 = load i32, ptr %merge_18
  %t28 = trunc i32 %t26 to i1
  %t29 = select i1 %t28, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t29)
  %t31 = add i32 0, 0
  %t32 = icmp slt i32 %t0, %t31
  store i32 1, ptr %merge_26
  br i1 %t32, label %bb8, label %bb7
bb7:
  %t33 = add i32 0, 10
  %t34 = icmp sgt i32 %t0, %t33
  %t35 = zext i1 %t34 to i32
  store i32 %t35, ptr %merge_26
  br label %bb8
bb8:
  %t36 = load i32, ptr %merge_26
  %t38 = trunc i32 %t36 to i1
  %t39 = select i1 %t38, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t39)
  ret i32 0
}

